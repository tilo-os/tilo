//! The workspace settings in a page: the session's own sheet, on its People tab, over the page.
//! Who is in the workspace and what each may do is what the workspace's host answers, and a role
//! changed or a person removed is asked of it; the tab is built from the members as the session's
//! shell builds it from its own ([`tilo_session::workspace_people`]).

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;
use std::rc::Rc;
use std::sync::Arc;

use tilo_desktop::here::Here;
use tilo_desktop::workspace_settings::people::{Member as Row, content_of};
use tilo_desktop_surface::i18n::t;
use tilo_relay_protocol::workspaces::{
    Member, Role, Workspace, WorkspacesReply, WorkspacesRequest,
};
use tilo_session::workspace_people;
use tilo_sheets::SheetState as _;
use tilo_sheets::workspace_settings::{self as sheet, WorkspaceSettingsEffect};

use crate::host_asks::{HostAsks, Reply, refused};
use crate::over_page::{OverSheet, PageSheet};

/// Why nothing was asked: a page with no workspace host has nobody to list.
const NO_HOST: &str = "this page is in no workspace";

/// The page's workspace settings, as the sheets' code makes it (`crate::over_page::SHEETS`).
pub(crate) fn make(
    shared: crate::desktop::Shared,
    here: Here,
    peer: Option<u32>,
) -> Box<dyn PageSheet> {
    Box::new(PageWorkspaceSettings::new(shared, here, peer))
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
}

impl PageWorkspaceSettings {
    fn new(shared: crate::desktop::Shared, here: Here, peer: Option<u32>) -> Self {
        Self {
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
            match effect {
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
                // The sheet closes itself; a page shows the People tab alone, and nothing else
                // the sheet could ask is drawn on it.
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
                    // The host answers roles, not the permissions beyond them.
                    grants: 0,
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
        sheet::Settings {
            title: t("workspace-settings-title"),
            name: tilo_desktop_surface::names::workspace(
                workspace.as_ref().map_or("", |workspace| &workspace.name),
            ),
            role: workspace_people::role_line(role),
            close: t("workspace-settings-close"),
            tabs: vec![workspace_people::tab_label(sheet::Tab::People)],
            tab: sheet::Tab::People,
            people,
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
