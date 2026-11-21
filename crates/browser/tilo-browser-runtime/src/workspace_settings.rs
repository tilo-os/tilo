//! The workspace settings in a page: the session's own sheet, on General, People and Agents, over
//! the page. General shows the workspace's name, editable as the workspace allows, the servers
//! keeping it, and leaving or deleting it; who is in the workspace and what each may do is what
//! its server answers, and a role changed or a person removed is asked of it; the tab is
//! built from the members as the session's shell builds it from its own
//! ([`tilo_session::workspace_people`]). Its agents come from Chat, read and changed in what
//! the page's windows keep, as the workspace lets this page's person
//! ([`tilo_session::workspace_settings::PageAgents`]).

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
use tilo_server_protocol::workspaces::{
    DocumentAccess, Member, Role, ServerEntry, Workspace, WorkspaceRoles, WorkspacesReply,
    WorkspacesRequest,
};
use tilo_session::workspace_people;
use tilo_session::workspace_settings::{AgentsSource, PageAgents, document_access_pane};
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
    pub stores: crate::workspace_resources::PageStores,
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
            stores: crate::workspace_resources::PageStores {
                owners: owners.clone(),
                stores: Rc::clone(&shared.stores),
                workspace: workspace.to_owned(),
                author: author.clone(),
                replicated: true,
            },
            permissions: permissions.clone(),
            administers: Arc::clone(&shared.administers),
            handle: handle.clone(),
            person: author,
        }
    }
}

impl AgentsSource for PageRecords {
    /// The page shows one workspace: its records are the ones it keeps.
    fn open(
        &self,
        _workspace: &tilo_desktop_model::Workspace,
        _person: &UserId,
    ) -> (WorkspaceResources, Option<LivePermissions>) {
        (
            self.stores.resources(),
            Some(LivePermissions::shared(self.permissions.clone())),
        )
    }

    fn manages(&self, _workspace: &tilo_desktop_model::Workspace, _person: &UserId) -> bool {
        self.administers.load(Ordering::Relaxed)
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
    roles: Rc<RefCell<Option<WorkspaceRoles>>>,
    /// The servers keeping the workspace, once its server answered its owner:
    /// `None` for a person who may not choose them, who sees nothing of them.
    servers: Rc<RefCell<Option<Vec<ServerEntry>>>>,
    documents: Rc<RefCell<DocumentAccess>>,
    refresh_documents: Rc<Cell<bool>>,
    /// The sheet waits for the workspace's first answer before it goes up, rather than show
    /// nobody in it.
    opening: Rc<Cell<bool>>,
    /// Whether the host was asked who is in the workspace since the sheet was last down.
    asked: bool,
    /// Set once the workspace's host holds a change this page made to its people: who is in it
    /// is asked again.
    relist: Rc<Cell<bool>>,
    /// Whether the server was asked which servers keep the workspace since the sheet was last down.
    asked_servers: bool,
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
                Rc::new(records) as Rc<dyn AgentsSource>,
                Arc::new(move || {
                    crate::desktop::wake(&woken);
                }),
            )
        });
        Self {
            agents,
            tab: Cell::new(sheet::Tab::General),
            state: sheet::WorkspaceSettingsState::default(),
            asks: HostAsks::new(peer, NO_HOST, Arc::clone(&shared.woken)),
            peer,
            shared,
            here,
            workspace: Rc::default(),
            roles: Rc::default(),
            servers: Rc::default(),
            documents: Rc::default(),
            refresh_documents: Rc::default(),
            opening: Rc::new(Cell::new(false)),
            asked: false,
            relist: Rc::default(),
            asked_servers: false,
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
        self.list_roles(uuid.clone());
        let then = self.taking();
        self.asks.ask(
            |_| WorkspacesRequest::List,
            move |reply| {
                then(match reply {
                    Ok(WorkspacesReply::Workspaces(list)) => list
                        .into_iter()
                        .find(|held| held.workspace == uuid)
                        .map(WorkspacesReply::Workspace)
                        .ok_or_else(|| "the server does not hold this workspace".to_owned()),
                    other => other,
                });
            },
        );
    }

    fn list_roles(&self, workspace: String) {
        let roles = Rc::clone(&self.roles);
        self.asks.ask(
            move |_| WorkspacesRequest::Roles { workspace },
            move |reply| {
                if let Ok(WorkspacesReply::Roles(answered)) = reply {
                    *roles.borrow_mut() = Some(answered);
                }
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
        let request = request(uuid, request_id);
        let changes_access = matches!(
            request,
            WorkspacesRequest::SetDocumentAccess { .. } | WorkspacesRequest::EditRoles { .. }
        );
        let refresh = Rc::clone(&self.refresh_documents);
        let take = self.taking();
        self.asks.ask(
            move |_| request,
            move |reply| {
                if changes_access
                    && matches!(
                        reply,
                        Ok(WorkspacesReply::Roles(_) | WorkspacesReply::Workspace(_))
                    )
                {
                    refresh.set(true);
                }
                take(reply);
            },
        );
    }

    /// Write `change` to the workspace's people as this page's own device, as the person's
    /// desktop writes it: the workspace's host relays it, and the sheet asks who is in the
    /// workspace again once the host holds it, or says why the workspace refused it.
    fn govern(&self, change: tilo_action_policy::Change) {
        let Some(peer) = self.peer else {
            return self.answered(Err(NO_HOST.to_owned()));
        };
        let changes_access = change.is_custom_role_change();
        let (relist, refresh, state, woken) = (
            Rc::clone(&self.relist),
            Rc::clone(&self.refresh_documents),
            self.state.clone(),
            Arc::clone(&self.shared.woken),
        );
        let written = crate::replicated::govern(
            peer,
            vec![change],
            Box::new(move |written| {
                match written {
                    Ok(()) => {
                        relist.set(true);
                        refresh.set(changes_access);
                    }
                    Err(why) => state.refuse(workspace_people::refusal(&why)),
                }
                crate::desktop::wake(&woken);
            }),
        );
        if let Err(why) = written {
            self.state.refuse(workspace_people::refusal(&why));
        }
    }

    fn list_documents(&self) {
        let Some(workspace) = self.uuid() else { return };
        let documents = Rc::clone(&self.documents);
        let state = self.state.clone();
        self.asks.ask(
            move |_| WorkspacesRequest::DocumentAccess { workspace },
            move |reply| match reply {
                Ok(WorkspacesReply::DocumentAccess(found)) => *documents.borrow_mut() = found,
                other => {
                    *documents.borrow_mut() = DocumentAccess::default();
                    state.refuse(workspace_people::refusal(&refused(other)));
                }
            },
        );
    }

    /// Ask the server which servers keep the workspace. A person who may not choose them is refused,
    /// and sees nothing of them: the sheet draws no servers then.
    fn list_servers(&mut self) {
        self.asked_servers = true;
        let Some(uuid) = self.uuid() else {
            *self.servers.borrow_mut() = None;
            return;
        };
        let servers = Rc::clone(&self.servers);
        self.asks.ask(
            move |_| WorkspacesRequest::Servers { workspace: uuid },
            move |reply| match reply {
                Ok(WorkspacesReply::Servers(list)) => *servers.borrow_mut() = Some(list),
                // Not an owner: the servers are the owner's to choose.
                _ => *servers.borrow_mut() = None,
            },
        );
    }

    /// Ask the server `request` to change which servers keep the workspace: its answer is the servers
    /// after the change, or why it refused, over the sheet.
    fn change_servers(&self, request: impl FnOnce(String) -> WorkspacesRequest + 'static) {
        let Some(uuid) = self.uuid() else {
            self.state
                .refuse(workspace_people::refusal("this page is in no workspace"));
            return;
        };
        let (servers, state) = (Rc::clone(&self.servers), self.state.clone());
        self.asks.ask(
            move |_| request(uuid),
            move |reply| {
                take_servers(&servers, &state, reply);
            },
        );
    }

    /// Who takes a host's answer: the workspace it answers with, or why it refused.
    fn taking(&self) -> impl FnOnce(Reply) + 'static {
        let (workspace, roles, state, opening) = (
            Rc::clone(&self.workspace),
            Rc::clone(&self.roles),
            self.state.clone(),
            Rc::clone(&self.opening),
        );
        move |reply| match reply {
            Ok(WorkspacesReply::Roles(answered)) => *roles.borrow_mut() = Some(answered),
            other => take(&workspace, &state, &opening, other),
        }
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
            self.follow(effect);
        }
        changed
    }

    /// Carry out one effect the person pressed, once the agents' pane has passed on it.
    fn follow(&self, effect: WorkspaceSettingsEffect) {
        match effect {
            WorkspaceSettingsEffect::ShowTab { tab } => {
                if let Some(tab) = sheet::Tab::from_key(&tab) {
                    self.tab.set(tab);
                    if tab == sheet::Tab::Permissions {
                        self.list_documents();
                    }
                }
            }
            WorkspaceSettingsEffect::SetDocumentAccess { .. }
            | WorkspaceSettingsEffect::SetRole { .. }
            | WorkspaceSettingsEffect::SaveCustomRole { .. }
            | WorkspaceSettingsEffect::SetCustomRole { .. }
            | WorkspaceSettingsEffect::RemoveCustomRole { .. }
            | WorkspaceSettingsEffect::RemoveMember { .. } => self.follow_member(effect),
            WorkspaceSettingsEffect::AddServer { .. }
            | WorkspaceSettingsEffect::RemoveServer { .. } => self.follow_servers(effect),
            // The workspace's server writes the name where every device and list of it reads
            // it; its answer is the workspace as it is after.
            WorkspaceSettingsEffect::Rename { name } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let Some(uuid) = self.uuid() else {
                    return;
                };
                self.asks.ask(
                    move |_| WorkspacesRequest::Rename {
                        workspace: uuid,
                        name,
                    },
                    self.taking(),
                );
            }
            // Taking the workspace off this person: they leave it, or it ends for everyone.
            WorkspaceSettingsEffect::Delete => self.leave(),
            WorkspaceSettingsEffect::DeleteWorkspace => self.delete(),
            WorkspaceSettingsEffect::DismissNotice => self.state.dismiss_notice(),
            // The sheet closes itself; a page draws no devices and no place among them to
            // move the workspace to, so nothing else the sheet could ask is drawn on it.
            _ => {}
        }
    }

    /// Carry out a change to who may do what, asked of the workspace's server.
    fn follow_member(&self, effect: WorkspaceSettingsEffect) {
        match effect {
            WorkspaceSettingsEffect::SetDocumentAccess {
                who,
                app,
                document,
                access,
            } => {
                self.change(
                    move |workspace, request_id| WorkspacesRequest::SetDocumentAccess {
                        workspace,
                        request_id,
                        app,
                        document,
                        recipient: who,
                        access,
                    },
                );
            }
            WorkspaceSettingsEffect::SetRole { member, role } => {
                let (Some(_), Some(role)) = (
                    person(&member),
                    Role::parse(&role).filter(|r| r.assignable()),
                ) else {
                    return;
                };
                // A role for someone in the workspace moves them to it, an administrator's
                // grants and all, as the desktop's sheet writes it.
                self.govern(tilo_action_policy::Change::SetRole {
                    person: member,
                    role,
                });
            }
            WorkspaceSettingsEffect::SaveCustomRole { role, name } => {
                let id = if role.is_empty() {
                    uuid::Uuid::new_v4().simple().to_string()
                } else {
                    role
                };
                self.govern(tilo_action_policy::Change::DefineRole { id, name });
            }
            WorkspaceSettingsEffect::SetCustomRole {
                member,
                role,
                assigned,
            } => {
                self.govern(tilo_action_policy::Change::SetMemberCustomRole {
                    person: member,
                    role,
                    assigned,
                });
            }
            WorkspaceSettingsEffect::RemoveCustomRole { role } => {
                self.govern(tilo_action_policy::Change::RemoveRole { id: role });
            }
            WorkspaceSettingsEffect::RemoveMember { member } => {
                if person(&member).is_none() {
                    return;
                }
                self.govern(tilo_action_policy::Change::RemoveMember { person: member });
            }
            _ => {}
        }
    }

    /// Carry out a change to the servers keeping the workspace, its server's answer being the
    /// servers after it.
    fn follow_servers(&self, effect: WorkspaceSettingsEffect) {
        match effect {
            WorkspaceSettingsEffect::AddServer { origin } => {
                let origin = origin.trim().to_owned();
                if !origin.is_empty() {
                    self.change_servers(move |workspace| WorkspacesRequest::AddServer {
                        workspace,
                        origin,
                    });
                }
            }
            WorkspaceSettingsEffect::RemoveServer { server } => {
                self.change_servers(move |workspace| WorkspacesRequest::RemoveServer {
                    workspace,
                    server,
                });
            }
            _ => {}
        }
    }

    /// The sheet as the workspace stands for this page's person.
    fn settings(&self) -> sheet::Settings {
        let workspace = self.workspace.borrow();
        let (mut people, role) = self.people();
        // What the workspace did is the session's to show; a page offers only the decision.
        people.remove_decision.review.clear();
        let general = self.general();
        // General is always there and comes first: the workspace's name is every page's
        // business, whatever else this workspace allows this person here.
        let mut tabs = vec![sheet::Tab::General, sheet::Tab::People];
        if self.agents.is_some() {
            tabs.push(sheet::Tab::Agents);
        }
        if people.role_editor.manage {
            tabs.push(sheet::Tab::Permissions);
        }
        let tab = Some(self.tab.get())
            .filter(|tab| tabs.contains(tab))
            .unwrap_or(sheet::Tab::General);
        let permissions = if tab == sheet::Tab::Permissions {
            self.permissions(&people)
        } else {
            sheet::Permissions::default()
        };
        sheet::Settings {
            title: t("workspace-settings-title"),
            name: tilo_desktop_surface::names::workspace(
                workspace.as_ref().map_or("", |workspace| &workspace.name),
            ),
            role: workspace_people::role_line(role),
            close: t("workspace-settings-close"),
            tabs: tabs.into_iter().map(workspace_people::tab_label).collect(),
            tab,
            general,
            people,
            permissions,
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

    /// The People tab as the workspace's members make it, and this page's person's own role.
    fn people(&self) -> (sheet::People, Option<Role>) {
        let workspace = self.workspace.borrow();
        let members: Vec<&Member> = workspace
            .iter()
            .flat_map(|workspace| &workspace.members)
            // Software holds no role, and whoever was removed is no longer in it.
            .filter(|member| member.role.is_some() && !member.removed)
            .collect();
        let own = members.iter().find(|member| member.is_self);
        let viewer = own
            .and_then(|member| member.id.person_id())
            .unwrap_or_default()
            .to_owned();
        let role = own.and_then(|member| member.role);
        let here = self.here.borrow();
        let mut rows: Vec<Row> = members
            .iter()
            .filter_map(|member| {
                let role = member.role?;
                let user = member.id.person_id()?.to_owned();
                Some(Row {
                    name: Some(member.name.trim())
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .or_else(|| here.person(&user).map(|(name, _)| name))
                        .unwrap_or_else(ui::presence::unnamed_person_label),
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
        let kept = workspace_people::holders(
            workspace
                .as_ref()
                .map_or(&[], |workspace| &workspace.holders),
        );
        // A page cannot sign a revocation: only the member's own device can, so
        // the sheet it builds offers none.
        let delegated = workspace_people::delegations(
            workspace
                .as_ref()
                .map_or(&[], |workspace| &workspace.delegations),
        );
        let mut content = content_of(&rows, &viewer, role == Some(Role::Owner), &kept, &delegated);
        let roles = self.roles.borrow();
        content.manage_roles &= roles.is_some();
        if let Some(governed) = roles.as_ref() {
            content.custom_roles.extend(
                governed
                    .custom_roles
                    .iter()
                    .map(|(id, name)| (id.clone(), name.clone())),
            );
            for person in &mut content.people {
                if let Some(roles) = governed.assigned_roles.get(&person.user) {
                    person.additional_roles.extend(roles.iter().cloned());
                }
            }
        }
        (workspace_people::tab(content), role)
    }

    /// The Permissions tab: who may reach which document, over the workspace's documents and
    /// the apps a page offers.
    fn permissions(&self, people: &sheet::People) -> sheet::Permissions {
        let recipients = people
            .custom_roles
            .iter()
            .map(|role| sheet::Choice {
                key: format!("role:{}", role.key),
                label: role.label.clone(),
            })
            .chain(people.people.iter().map(|person| sheet::Choice {
                key: format!("member:{}", person.key),
                label: person.name.clone(),
            }))
            .collect();
        let documents = self.documents.borrow();
        let targets = documents
            .documents
            .iter()
            .cloned()
            .chain(
                tilo_desktop_search::availability::catalog()
                    .iter()
                    .filter(|app| app.platforms.contains(&"web"))
                    .map(|app| (String::new(), app.id.to_owned())),
            )
            .chain(std::iter::once((String::new(), String::new())))
            .collect();
        sheet::Permissions {
            composer: sheet::Composer {
                who: t("workspace-settings-who"),
                ..sheet::Composer::default()
            },
            documents: Some(document_access_pane(
                targets,
                documents.rules.clone(),
                recipients,
            )),
            ..sheet::Permissions::default()
        }
    }

    /// Everything the General tab shows: the workspace's name, editable where the workspace lets
    /// this person rename it; the servers keeping it; leaving it; and deleting it, where it is
    /// theirs with nobody else in it.
    fn general(&self) -> sheet::General {
        let workspace = self.workspace.borrow();
        let members: Vec<&Member> = workspace
            .iter()
            .flat_map(|workspace| &workspace.members)
            // Software holds no role, and whoever was removed is no longer in it.
            .filter(|member| member.role.is_some() && !member.removed)
            .collect();
        let own = members.iter().find(|member| member.is_self).copied();
        let role = own.and_then(|member| member.role);
        let administrator = own.is_some_and(|member| member.administrator);
        // Renaming is the workspace's owner's and moderators', and whoever it made an
        // administrator.
        let can_rename = matches!(role, Some(Role::Owner | Role::Moderator)) || administrator;
        // A member leaves by taking themselves out; the owner's place is the workspace's own.
        let leavable = role.is_some_and(|role| role != Role::Owner);
        // Deleting ends the workspace for everyone, so it is its owner's alone to do, with nobody
        // else left in it.
        let deletable = role == Some(Role::Owner) && members.len() <= 1;
        let name = tilo_desktop_surface::names::workspace(
            workspace.as_ref().map_or("", |workspace| &workspace.name),
        );
        sheet::General {
            identity: sheet::Group {
                title: t("workspace-settings-identity-title"),
                description: t("workspace-settings-identity-description"),
            },
            name_label: t("workspace-settings-name-label"),
            can_rename,
            rename_note: (!can_rename).then(|| t("workspace-settings-rename-admin-only")),
            servers: self.servers(),
            leave: if leavable {
                sheet::Group {
                    title: t("workspace-settings-leave"),
                    description: t("workspace-settings-leave-membership"),
                }
            } else {
                sheet::Group::default()
            },
            leave_label: t("workspace-settings-leave-action"),
            leavable,
            delete: deletable.then(|| sheet::Group {
                title: t("workspace-settings-delete"),
                description: t("workspace-settings-delete-description"),
            }),
            delete_label: t("workspace-settings-delete-action"),
            delete_decision: sheet::Decision {
                title: tilo_sheets::words("workspace-delete-title", &[("name", name.as_str())]),
                description: tilo_sheets::word("workspace-delete-body"),
                keep: tilo_sheets::word("workspace-delete-keep"),
                review: String::new(),
                confirm: tilo_sheets::word("workspace-delete-confirm"),
            },
            ..sheet::General::default()
        }
    }

    /// Which servers keep the workspace, its owners' to choose: the page shows them the tab
    /// once the server answered with them, and a person who may not choose sees nothing of them.
    fn servers(&self) -> Option<sheet::Servers> {
        let servers = self.servers.borrow();
        let servers = servers.as_ref()?;
        let here = self.here.borrow();
        Some(sheet::Servers {
            group: sheet::Group {
                title: t("workspace-settings-servers-title"),
                description: t("workspace-settings-servers-description"),
            },
            origin_label: t("workspace-settings-servers-origin-label"),
            add_label: t("workspace-settings-servers-add"),
            remove_label: t("workspace-settings-servers-remove"),
            rows: servers
                .iter()
                .map(|server| {
                    let label =
                        server_label(server, here.person(&server.person).map(|(name, _)| name));
                    sheet::Server {
                        key: server.device.clone(),
                        // A server named by its own name says it is the one the page is on
                        // beside it; one read as "This server" already said so.
                        note: (server.here && !site_name(&server.label).is_empty())
                            .then(|| t("workspace-settings-servers-connected")),
                        label,
                        here: server.here,
                    }
                })
                .collect(),
        })
    }

    /// A member takes themselves out of the workspace: its server answers with the workspace as
    /// it is after, and the page goes on in another of their workspaces.
    fn leave(&self) {
        let Some(uuid) = self.uuid() else {
            return;
        };
        let own = self.workspace.borrow().as_ref().and_then(|workspace| {
            workspace
                .members
                .iter()
                .find(|member| member.is_self && !member.removed)
                .and_then(|member| member.id.person_id())
                .and_then(person)
        });
        let Some(user_public_key) = own else {
            return;
        };
        let mut request_id = [0u8; 32];
        if let Err(error) = getrandom::fill(&mut request_id) {
            return self.answered(Err(error.to_string()));
        }
        let then = self.went(uuid.clone());
        self.asks.ask(
            move |_| WorkspacesRequest::Remove {
                workspace: uuid,
                request_id,
                user_public_key,
            },
            then,
        );
    }

    /// Delete the workspace, with everything in it: its server ends it, and the page goes on in
    /// another of the person's workspaces.
    fn delete(&self) {
        let Some(uuid) = self.uuid() else {
            return;
        };
        let then = self.went(uuid.clone());
        self.asks
            .ask(move |_| WorkspacesRequest::Delete { workspace: uuid }, then);
    }

    /// Take the server's answer to leaving or deleting `workspace`: the workspace after a leave,
    /// or that it was deleted, and the page moves on to another of the person's; or why it
    /// refused, over the sheet.
    fn went(&self, left: String) -> impl FnOnce(Reply) + 'static {
        let (workspace, state, shared) = (
            Rc::clone(&self.workspace),
            self.state.clone(),
            self.shared.clone(),
        );
        move |reply| match reply {
            Ok(WorkspacesReply::Workspace(answered)) => {
                *workspace.borrow_mut() = Some(answered);
                left_for(&state, &shared, &left);
            }
            Ok(WorkspacesReply::Deleted) => left_for(&state, &shared, &left),
            other => state.refuse(workspace_people::refusal(&refused(other))),
        }
    }
}

/// The page left `workspace`: the sheet closes, and the page opens another of the person's
/// workspaces, or its own entry when they have none left.
fn left_for(
    state: &sheet::WorkspaceSettingsState,
    shared: &crate::desktop::Shared,
    workspace: &str,
) {
    state.presentation().close();
    shared
        .requests
        .borrow_mut()
        .push(crate::shell::HostRequest::Action(format!(
            "left-workspace:{workspace}"
        )));
    shared.wake();
}

/// A server's name as people read it: the site it names itself by, without the port a
/// machine dials it at (`tilo.localhost:26420` reads `tilo.localhost`).
fn site_name(label: &str) -> &str {
    let label = label.trim();
    match label.rsplit_once(':') {
        Some((site, port)) if !site.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) => site,
        _ => label,
    }
}

/// What a server keeping the workspace is called: by the name it gives itself; else the one the
/// page is connected to as "This server", any other by whom it keeps the workspace for.
fn server_label(server: &ServerEntry, kept_for: Option<String>) -> String {
    let named = site_name(&server.label);
    if !named.is_empty() {
        return named.to_owned();
    }
    if server.here {
        return t("workspace-settings-servers-here");
    }
    kept_for.unwrap_or_else(|| t("workspace-settings-servers-unnamed"))
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

/// Take the server's answer about the servers keeping the workspace: the servers after the change,
/// or why it refused, over the sheet.
fn take_servers(
    servers: &RefCell<Option<Vec<ServerEntry>>>,
    state: &sheet::WorkspaceSettingsState,
    reply: Reply,
) {
    match reply {
        Ok(WorkspacesReply::Servers(list)) => *servers.borrow_mut() = Some(list),
        other => state.refuse(workspace_people::refusal(&refused(other))),
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
        // The workspace's name is what the sheet is opened for: it opens on General.
        self.tab.set(sheet::Tab::General);
        if self.state.presentation().is_open() {
            return;
        }
        self.opening.set(true);
        if self.tab.get() == sheet::Tab::Permissions {
            self.list_documents();
        }
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
        if up && self.tab.get() == sheet::Tab::Permissions && self.refresh_documents.replace(false)
        {
            self.list_documents();
        }
        // Opening, or the workspace's people moved while the sheet is up: ask who is in it now.
        if up
            && (!self.asked
                || self.relist.replace(false)
                || self.asked_for != *self.shared.members.borrow())
        {
            self.list();
        }
        // Which servers keep the workspace: whether the person may choose them is the server's to
        // say, so it is asked once each time the sheet comes up.
        if up && !self.asked_servers {
            self.list_servers();
        }
        changed |= self.follow_asked();
        if !self.is_open() {
            if !self.opening.get() {
                self.asked = false;
                self.asked_servers = false;
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

    fn put_away(&self) {
        let presentation = self.state.presentation();
        if !presentation.is_open() {
            presentation.gone();
        }
    }

    fn drawn(&self) -> bool {
        self.state.presentation().is_drawn()
    }

    fn sheet_name(&self) -> &'static str {
        "PageWorkspaceSettings"
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_server_reads_by_its_site_without_the_port_it_is_dialled_at() {
        assert_eq!(super::site_name(" tilo.localhost:26420 "), "tilo.localhost");
        assert_eq!(super::site_name("eu1.tilo.sh"), "eu1.tilo.sh");
        assert_eq!(super::site_name("Ada's laptop"), "Ada's laptop");
        assert_eq!(super::site_name(""), "");
    }
}
