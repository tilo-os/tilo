//! Workspace settings on a computer: what the sheet shows of the workspace in front, and what the
//! person's choices there change.
//! The sheet shows and asks; this carries out. Every change to the workspace goes through the
//! workspace's own policy functions — the ones the shell's member commands use — so a change the
//! person may not make is refused there, whatever the sheet showed, and the tree is only written
//! when something changed.

use std::cell::{Cell, RefCell};

use collab_types::caps::Effect;
use tilo_action_policy::{Refusal, Role};
use tilo_desktop_model::{Layout, Workspace};
use tilo_session_core::desktop::authority::WorkspaceAuthority as _;
use tilo_session_core::desktop::host::chrome::ChromeIntent;
use tilo_session_core::desktop::i18n::{t, t_args};
use tilo_session_core::desktop::people::PeopleNames;
use tilo_session_core::desktop::state::DesktopHandle;
use tilo_session_core::desktop::workspace_settings as domain;
use tilo_sheets::SheetState as _;
use tilo_sheets::workspace_settings as sheet;

use crate::engine::Engine;
use crate::lists::AppLists;
use crate::search::SessionSearch;
use crate::workspace_view::{Holder, WorkspaceView};
use crate::workspaces::{DeviceChoice, Personal};

use super::{Directory, agents, emoji};

pub(crate) use agents::Agents as AgentsTab;
pub(crate) use emoji::Emoji as EmojiTab;

/// The workspace's own resources the sheet reads: its agents and its emoji,
/// both kept by Chat for every app of the workspace.
pub(crate) struct WorkspaceTabs {
    pub(crate) agents: AgentsTab,
    pub(crate) emoji: EmojiTab,
    /// The workspaces' resources, as this device's daemon reads them for the person: the ones a
    /// rule of the workspace's can reach.
    pub(crate) documents: std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
}

impl WorkspaceTabs {
    /// The two resources, followed through the device's daemon, waking the session's loop
    /// through `wake` when a change lands; `service_root` is where this device's service keeps
    /// what the agents tab reads of the models it runs.
    pub(crate) fn new(
        daemon: &tilo_session_client::daemon_documents::DaemonShell,
        service_root: Option<std::path::PathBuf>,
        documents: std::sync::Arc<dyn tilo_peer_state::workspace_documents::DeviceDocuments>,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            agents: AgentsTab::new(daemon.clone(), service_root, wake.clone()),
            emoji: EmojiTab::new(daemon.clone(), wake),
            documents,
        }
    }
}

/// What the sheet is fed from: the shell in front, the lists its tabs offer,
/// and the workspace's own records its tabs read.
pub(crate) struct Feeding<'a> {
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) person: &'a str,
    /// This session's search: the machine's app lists, and the apps a page or a link the sheet asks
    /// for opens in.
    pub(crate) search: &'a SessionSearch,
    /// The shell's revision and the workspace state's the panes were last fed
    /// at.
    pub(crate) fed: &'a Cell<Option<(u64, u64)>>,
    /// The workspace's agents and emoji, read and changed through Chat's records.
    pub(crate) tabs: &'a WorkspaceTabs,
    /// Where the person is asked for a picture to add.
    pub(crate) asks: Option<&'a tilo_service::AskSender>,
    pub(crate) personal: Option<&'a RefCell<Personal>>,
    /// This device's engine, where what this device may read of the workspace's records is
    /// decided.
    pub(crate) engine: Option<&'a std::sync::Arc<Engine>>,
    /// How the person's workspaces stand, as the device's daemon publishes them: what the sheet
    /// shows of the workspace's people and hosts, and where the person's changes to them go.
    pub(crate) view: Option<&'a std::sync::Arc<WorkspaceView>>,
    /// Where what the workspace answered a change is taken in, once its signature landed.
    pub(crate) landings: &'a crate::landing::Landings,
}

/// Open the sheet on the workspace in front: its panes as the workspace stands, and its name field
/// ready for a new name.
pub(crate) fn open(state: Option<&sheet::WorkspaceSettingsState>, fed: &Feeding<'_>) {
    let Some(state) = state else {
        return;
    };
    show(state, fed, sheet::Tab::General, None);
    state.reset();
    state.presentation().open(false);
}

/// Open the sheet on the workspace `workspace` asks for, on `route`'s tab:
/// what an app naming a route asks of the person's own settings.
///
/// # Errors
/// The person is looking at another workspace — a request never switches
/// what they see — or this session shows no workspace settings.
pub(crate) fn open_route(
    state: Option<&sheet::WorkspaceSettingsState>,
    fed: &Feeding<'_>,
    workspace: &str,
    route: runtime_portal::tools::WorkspaceSettingsRoute,
) -> Result<(), String> {
    let state = state.ok_or("this session shows no workspace settings")?;
    let front = fed
        .handle
        .read(|tree| tree.active_workspace().map(|front| front.id().to_string()));
    if front.as_deref() != Some(workspace) {
        return Err("workspace settings opens for the workspace in front".to_owned());
    }
    show(state, fed, tab_of(route), None);
    state.reset();
    state.presentation().open(false);
    Ok(())
}

/// The tab a route names, as the sheet's own tabs are keyed.
fn tab_of(route: runtime_portal::tools::WorkspaceSettingsRoute) -> sheet::Tab {
    use runtime_portal::tools::WorkspaceSettingsRoute;
    match route {
        WorkspaceSettingsRoute::General => sheet::Tab::General,
        WorkspaceSettingsRoute::Members => sheet::Tab::People,
        WorkspaceSettingsRoute::PrivacySecurity => sheet::Tab::Permissions,
        WorkspaceSettingsRoute::AppsFiles => sheet::Tab::Apps,
        WorkspaceSettingsRoute::Emoji => sheet::Tab::Emoji,
    }
}

/// Keep the sheet's panes current while it is up: the workspace changes while the sheet is open (a
/// role set, a grant given), and so does what other people do in it — the workspace's emoji among
/// them.
pub(crate) fn refresh(state: Option<&sheet::WorkspaceSettingsState>, fed: &Feeding<'_>) {
    let Some(state) = state else {
        return;
    };
    if !state.presentation().is_open() {
        return;
    }
    let shown = state.settings();
    let emoji_moved = shown.tab == sheet::Tab::Emoji
        && fed
            .handle
            .read(|tree| fed.tabs.emoji.moved(tree, fed.person));
    let agents_moved = shown.tab == sheet::Tab::Agents
        && fed
            .handle
            .read(|tree| fed.tabs.agents.moved(tree, fed.person));
    if !emoji_moved && !agents_moved && fed.fed.get() == Some(fed_revision(fed)) {
        return;
    }
    let review = shown
        .agents
        .review
        .as_ref()
        .map(|review| review.agent.clone());
    show(state, fed, shown.tab, review.as_deref());
}

/// What the sheet's panes are built from, as it stands: the shell the person
/// sees, and the workspace state every member's change moves.
fn fed_revision(fed: &Feeding<'_>) -> (u64, u64) {
    (
        fed.handle.revision(),
        fed.view.map_or(0, |view| view.revision()),
    )
}

/// Show the sheet's panes as the shell stands for the person, on `tab` .
fn show(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    tab: sheet::Tab,
    review: Option<&str>,
) {
    let tree = fed.handle.read(Clone::clone);
    let apps = Directory {
        actions: fed.search.apps(),
        workspace: tree
            .active_workspace()
            .map_or_else(String::new, |ws| ws.id().to_string()),
    };
    let mut shown = settings(
        &tree,
        fed.person,
        &apps,
        tab,
        review,
        fed.personal,
        fed.tabs,
        (
            fed.view.map(|view| &**view),
            fed.engine.map(|engine| &**engine),
        ),
    );
    shown.notice = state.notice();
    state.show(shown);
    fed.fed.set(Some(fed_revision(fed)));
}

/// Show the workspace in front on one of the person's devices, or keep it off there: the row says
/// the choice at once, since a workspace kept off this device leaves the shell under the sheet.
fn toggle_workspace_device(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>, device: &str) {
    if !toggle_device(fed.handle, fed.personal, device) {
        return;
    }
    let shown = state.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(state, fed, shown.tab, review.as_deref());
}

/// Show one agent's review, or the roster again for `None` , on the tab the sheet was on.
fn show_review(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>, agent: Option<&str>) {
    show(state, fed, state.settings().tab, agent);
}

/// Show the tab the person picked, and keep it for the next opening.
fn show_picked_tab(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>, tab: &str) {
    let Some(tab) = sheet::Tab::from_key(tab) else {
        return;
    };
    let shown = state.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(state, fed, tab, review.as_deref());
}

/// Carry out what the person asked for in the sheet.
pub(crate) fn follow(
    state: Option<&sheet::WorkspaceSettingsState>,
    fed: &Feeding<'_>,
    intents: &RefCell<Vec<ChromeIntent>>,
) {
    let Some(state) = state else {
        return;
    };
    let handle = fed.handle;
    let person = fed.person;
    // What came of the chooser and of the pictures waiting for the image tools, and what the
    // agents' record owes this device, before what the person asks now.
    handle.read(|tree| fed.tabs.emoji.settle(tree, person));
    handle.read(|tree| fed.tabs.agents.settle(tree, person));
    for effect in state.take_asked() {
        ask(state, fed, intents, effect);
    }
}

/// Carry out the one ask `effect` names. Every ask the sheet can make is answered here, so an
/// effect added to the sheet is a compile error until the session says what it does with it.
fn ask(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    intents: &RefCell<Vec<ChromeIntent>>,
    effect: sheet::WorkspaceSettingsEffect,
) {
    let handle = fed.handle;
    let person = fed.person;
    match effect {
        sheet::WorkspaceSettingsEffect::ShowTab { tab } => {
            show_picked_tab(state, fed, &tab);
        }
        sheet::WorkspaceSettingsEffect::DismissNotice => dismiss_notice(state, fed),
        // Closing is the sheet's own. The servers keeping a workspace are
        // chosen over the workspace's own server API, from a page; a
        // desktop's sheet offers none.
        sheet::WorkspaceSettingsEffect::Close
        | sheet::WorkspaceSettingsEffect::AddServer { .. }
        | sheet::WorkspaceSettingsEffect::RemoveServer { .. } => {}
        sheet::WorkspaceSettingsEffect::AddAgent { .. }
        | sheet::WorkspaceSettingsEffect::RemoveAgent { .. }
        | sheet::WorkspaceSettingsEffect::SetAgentModel { .. }
        | sheet::WorkspaceSettingsEffect::SetAgentSpendLimit { .. }
        | sheet::WorkspaceSettingsEffect::SetAgentAnswer { .. }
        | sheet::WorkspaceSettingsEffect::ResetAgentApp { .. } => agent_ask(state, fed, &effect),
        sheet::WorkspaceSettingsEffect::ToggleDevice { device } => {
            toggle_workspace_device(state, fed, &device);
        }
        sheet::WorkspaceSettingsEffect::Rename { name } => {
            intent(intents, ChromeIntent::RenameWorkspace { name });
        }
        sheet::WorkspaceSettingsEffect::Move { step } => move_workspace(handle, step),
        sheet::WorkspaceSettingsEffect::SetRole { .. }
        | sheet::WorkspaceSettingsEffect::SaveCustomRole { .. }
        | sheet::WorkspaceSettingsEffect::SetCustomRole { .. }
        | sheet::WorkspaceSettingsEffect::RemoveCustomRole { .. }
        | sheet::WorkspaceSettingsEffect::SetDocumentAccess { .. }
        | sheet::WorkspaceSettingsEffect::RemoveMember { .. }
        | sheet::WorkspaceSettingsEffect::Grant { .. }
        | sheet::WorkspaceSettingsEffect::Revoke { .. } => governance_ask(state, fed, effect),
        sheet::WorkspaceSettingsEffect::ReviewActivity => {
            intent(intents, ChromeIntent::ShowActivity);
        }
        sheet::WorkspaceSettingsEffect::Invite => intent(intents, ChromeIntent::Invite),
        sheet::WorkspaceSettingsEffect::NextWorkspaceDefault { resource } => {
            next_workspace_default(state, fed, &resource);
        }
        sheet::WorkspaceSettingsEffect::NextPersonalDefault { resource } => {
            next_personal_default(state, fed, &resource);
        }
        // The workspace leaves this device's desktop; it stays for everyone in it, on their
        // devices.
        sheet::WorkspaceSettingsEffect::Delete => intent(intents, ChromeIntent::LeaveWorkspace),
        // The person answered the sheet's own question: the shell deletes the workspace with
        // everything in it, and tells the session it went.
        sheet::WorkspaceSettingsEffect::DeleteWorkspace => {
            intent(intents, ChromeIntent::DeleteWorkspaceConfirmed);
        }
        sheet::WorkspaceSettingsEffect::AddEmoji
        | sheet::WorkspaceSettingsEffect::RemoveEmoji { .. }
        | sheet::WorkspaceSettingsEffect::DismissEmojiNotice => {
            handle.read(|tree| fed.tabs.emoji.asked(&effect, tree, person, fed.asks));
        }
        sheet::WorkspaceSettingsEffect::ReviewAgent { agent } => {
            show_review(state, fed, Some(&agent));
        }
        sheet::WorkspaceSettingsEffect::EndReview => show_review(state, fed, None),
    }
}

fn governance_ask(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    effect: sheet::WorkspaceSettingsEffect,
) {
    match effect {
        sheet::WorkspaceSettingsEffect::SetRole { member, role } => {
            let Some(role) = Role::parse(&role) else {
                tracing::debug!(%role, "no such role: nothing changed");
                return;
            };
            govern(state, fed, move |ws, actor| {
                domain::set_member_role(ws, actor, &member, role)
            });
        }
        sheet::WorkspaceSettingsEffect::SaveCustomRole { role, name } => {
            let id = if role.is_empty() {
                uuid::Uuid::new_v4().simple().to_string()
            } else {
                role
            };
            custom_role_change(
                state,
                fed,
                tilo_action_policy::Change::DefineRole { id, name },
            );
        }
        sheet::WorkspaceSettingsEffect::SetCustomRole {
            member,
            role,
            assigned,
        } => {
            custom_role_change(
                state,
                fed,
                tilo_action_policy::Change::SetMemberCustomRole {
                    person: member,
                    role,
                    assigned,
                },
            );
        }
        sheet::WorkspaceSettingsEffect::RemoveCustomRole { role } => {
            custom_role_change(
                state,
                fed,
                tilo_action_policy::Change::RemoveRole { id: role },
            );
        }
        sheet::WorkspaceSettingsEffect::SetDocumentAccess {
            who,
            app,
            document,
            access,
        } => set_document_access(state, fed, (who, app, document), access),
        sheet::WorkspaceSettingsEffect::RemoveMember { member } => {
            govern(state, fed, move |ws, actor| {
                domain::remove_member(ws, actor, &member)
            });
        }
        sheet::WorkspaceSettingsEffect::Grant {
            subject,
            permission,
            until,
        } => govern(state, fed, move |ws, actor| {
            let now = tilo_clock::unix_now_ms();
            let Some(draft) = domain::draft_for(ws, &subject, &permission, until, now) else {
                tracing::debug!(%subject, %permission, "no such grant: nothing changed");
                return Ok(domain::PolicyEdit::default());
            };
            domain::apply_grant_draft(ws, actor, &draft)
        }),
        sheet::WorkspaceSettingsEffect::Revoke { grant } => {
            govern(state, fed, move |ws, actor| {
                domain::apply_revoke(ws, actor, &grant)
            });
        }
        _ => tracing::error!("received a non-governance workspace setting"),
    }
}

fn set_document_access(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    (who, app, document): (String, String, String),
    access: String,
) {
    if tilo_action_policy::roles::is_private_record(&app, &document) {
        refuse(state, fed, "this document belongs to a person");
        return;
    }
    govern(state, fed, move |ws, actor| {
        domain::set_document_access(ws, actor, (&who, &app, &document), &access)
    });
}

fn custom_role_change(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    change: tilo_action_policy::Change,
) {
    govern(state, fed, move |ws, actor| {
        ws.policy
            .change_all_as_at(actor, vec![change.clone()], tilo_clock::unix_now())
    });
}

/// Put the sheet's notice away and draw the tab it stands over again, review and all.
fn dismiss_notice(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>) {
    state.dismiss_notice();
    let shown = state.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(state, fed, shown.tab, review.as_deref());
}

/// Say what the shell does next, for the sheet.
fn intent(intents: &RefCell<Vec<ChromeIntent>>, what: ChromeIntent) {
    intents.borrow_mut().push(what);
}

/// Say over the sheet why the workspace refused a change it answered after the pass that asked:
/// the panes stand as they were, and the note goes over them.
fn refused(state: &sheet::WorkspaceSettingsState, reason: &str) {
    state.refuse(crate::workspace_people::refusal(reason));
    let mut shown = state.settings();
    shown.notice = state.notice();
    state.show(shown);
}

/// Apply `edit` to the workspace in front as `person` , and keep the tree when it changed
/// something: the policy functions refuse a change the person may not make, and a refusal changes
/// nothing.
fn edit(
    handle: &DesktopHandle,
    person: &str,
    edit: impl FnOnce(&mut Workspace, &str) -> domain::PolicyEdit,
) {
    edit_changed(handle, person, |workspace, actor| {
        !edit(workspace, actor).is_empty()
    });
}

/// A change to the workspace in front, made where the workspace keeps it
/// ([`crate::governance::govern`]); one it refuses leaves the shell as it was, and the sheet says why
/// in the workspace's own words.
fn govern(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    edit: impl Fn(&mut Workspace, &str) -> Result<domain::PolicyEdit, Refusal> + 'static,
) {
    let Some(workspace) = fed
        .handle
        .read(|tree| tree.active_workspace().map(Workspace::id))
    else {
        return;
    };
    let at = crate::governance::Governing {
        handle: fed.handle,
        person: fed.person,
        view: fed.view,
        landings: fed.landings,
    };
    let landed_state = state.clone();
    let landed = move |landed: Result<(), String>| match landed {
        // The change the person made is the one that stands, so a refusal they have since
        // corrected has nothing left to say.
        Ok(()) => landed_state.dismiss_notice(),
        Err(reason) => refused(&landed_state, &reason),
    };
    if let Err(refusal) = crate::governance::govern(&at, workspace, edit, landed) {
        refuse(state, fed, &refusal.to_string());
    }
}

/// Say why the workspace refused a change, over the tab the person asked on,
/// and leave the workspace as it was.
fn refuse(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>, reason: &str) {
    state.refuse(crate::workspace_people::refusal(reason));
    let shown = state.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(state, fed, shown.tab, review.as_deref());
}

/// Apply `change` to the workspace in front as `person` , and keep the tree when it answers that
/// something changed.
fn edit_changed(
    handle: &DesktopHandle,
    person: &str,
    change: impl FnOnce(&mut Workspace, &str) -> bool,
) {
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    if !change(workspace, person) {
        return;
    }
    handle.replace_tree(tree);
}

/// Move what opens a kind of file or link in the workspace on to the next app that can.
fn next_workspace_default(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    resource: &str,
) {
    let Some(resource) = desktop_policy::ResourceKind::parse(resource) else {
        tracing::debug!(%resource, "no such kind of file or link: nothing changed");
        return;
    };
    let lists = crate::app_icons::app_lists().unwrap_or_default();
    let candidates = lists.candidates.clone();
    let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
    let handle = fed.handle;
    let person = fed.person;
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    let next = domain::next_workspace_default(
        workspace.default_apps.policy.get(&resource),
        &resource,
        &candidates,
    );
    if workspace.default_apps.policy.get(&resource) == Some(&next) {
        return;
    }
    let changed = workspace.set_default_app_policy_as(person, resource, next, &resolver);
    match changed {
        Ok(()) => handle.replace_tree(tree),
        // The workspace turned the change down: the sheet says why where the person chose the app.
        Err(error) => refuse(state, fed, &error.to_string()),
    }
}

/// Move what opens a kind of file or link for `person` themselves on to the next app that can.
fn next_personal_default(state: &sheet::WorkspaceSettingsState, fed: &Feeding<'_>, resource: &str) {
    let Some(resource) = desktop_policy::ResourceKind::parse(resource) else {
        tracing::debug!(%resource, "no such kind of file or link: nothing changed");
        return;
    };
    let lists = crate::app_icons::app_lists().unwrap_or_default();
    let candidates = lists.candidates.clone();
    let resolver = desktop_policy::DefaultAppResolver::new(&candidates);
    let handle = fed.handle;
    let person = fed.person;
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    let current = workspace
        .default_apps
        .user_choices
        .get(person)
        .and_then(|choices| choices.get(&resource));
    let next = domain::next_user_default(current, &resource, &candidates);
    if current == Some(&next) {
        return;
    }
    let changed = workspace.set_default_app_choice_as(person, person, resource, next, &resolver);
    match changed {
        Ok(()) => handle.replace_tree(tree),
        // The person's own choice is theirs to make; a change the workspace does not let them keep
        // says why on the sheet.
        Err(error) => refuse(state, fed, &error.to_string()),
    }
}

/// The workspace in front, as the record of its agents is opened against.
fn workspace_in_front(fed: &Feeding<'_>) -> Option<Workspace> {
    fed.handle.read(|tree| tree.active_workspace().cloned())
}

impl Feeding<'_> {
    /// Make an agent called `name` in the workspace `workspace_id` names (the
    /// one in front when it names none), kept by the person: the record is
    /// the agent's admission, and Chat's grants decide who may make one.
    ///
    /// # Errors
    /// The workspace is not open here, or Chat's grants refuse the record.
    pub(crate) fn make_agent(
        &self,
        workspace_id: Option<&str>,
        name: &str,
        model: &str,
    ) -> Result<String, String> {
        let workspace = self.workspace(workspace_id)?;
        self.tabs
            .agents
            .create(Some(&workspace), self.person, (name, None), model)
            .map(|(id, _)| id)
    }

    /// The person's agent for the MCP client `client` in the workspace `workspace_id` names: the
    /// one the workspace's record lists, else a new one, with the name the record keeps for it. A
    /// client the person approved is one agent member, whichever connection it comes in on.
    ///
    /// # Errors
    /// The workspace is not open here, or Chat's grants refuse the record.
    pub(crate) fn mcp_agent(
        &self,
        workspace_id: &str,
        client: &str,
    ) -> Result<(String, String), String> {
        let workspace = self.workspace(Some(workspace_id))?;
        if let Some(agent) = self
            .tabs
            .agents
            .of_client(Some(&workspace), self.person, client)
        {
            return Ok(agent);
        }
        self.tabs
            .agents
            .create(Some(&workspace), self.person, (client, Some(client)), "")
    }

    fn workspace(&self, workspace_id: Option<&str>) -> Result<Workspace, String> {
        let tree = self.handle.read(Clone::clone);
        let at = match workspace_id {
            Some(id) => tree
                .workspaces
                .iter()
                .position(|held| held.id().to_string() == id),
            None => tree
                .workspaces
                .iter()
                .position(|held| held.id() == tree.active),
        };
        at.and_then(|at| tree.workspaces.get(at))
            .cloned()
            .ok_or_else(|| t("workspace-settings-agents-unavailable"))
    }
}

/// Carry out an ask of the Agents tab, the record's own action, and show the tab as it then stands.
/// Retiring an agent also takes back what the workspace admitted it with: it takes no new work
/// here, on any device.
fn agent_ask(
    state: &sheet::WorkspaceSettingsState,
    fed: &Feeding<'_>,
    effect: &sheet::WorkspaceSettingsEffect,
) {
    let workspace = workspace_in_front(fed);
    let Some(answered) = fed
        .tabs
        .agents
        .answer(workspace.as_ref(), fed.person, effect)
    else {
        return;
    };
    match answered {
        agents::Answered::Show(review) => {
            if let sheet::WorkspaceSettingsEffect::RemoveAgent { agent } = effect {
                edit(fed.handle, fed.person, |ws, actor| {
                    domain::remove_agent(ws, actor, agent)
                });
            }
            show(state, fed, state.settings().tab, review.as_deref());
        }
        agents::Answered::Done => {}
        agents::Answered::Refused(reason) => refuse(state, fed, &reason),
    }
}

/// Put the workspace in front one place earlier or later among this device's workspaces.
fn move_workspace(handle: &DesktopHandle, step: i32) {
    let mut tree = handle.read(Clone::clone);
    let Some(at) = tree
        .workspaces
        .iter()
        .position(|workspace| workspace.id() == tree.active)
    else {
        return;
    };
    let last = tree.workspaces.len().saturating_sub(1);
    let to = if step < 0 {
        at.saturating_sub(usize::try_from(step.unsigned_abs()).unwrap_or(usize::MAX))
    } else {
        at.saturating_add(usize::try_from(step).unwrap_or(usize::MAX))
            .min(last)
    };
    if to != at {
        tree.workspaces.swap(at, to);
        handle.replace_tree(tree);
    }
}

/// Everything the sheet shows of `tree` for `person` .
fn settings(
    tree: &Layout,
    person: &str,
    apps: &dyn domain::AppDirectory,
    tab: sheet::Tab,
    review: Option<&str>,
    personal: Option<&RefCell<Personal>>,
    records: &WorkspaceTabs,
    (view, engine): (Option<&WorkspaceView>, Option<&Engine>),
) -> sheet::Settings {
    let (agents, emoji) = (&records.agents, &records.emoji);
    let workspace = tree.active_workspace();
    let names = PeopleNames::of(tree, person, own_name().as_deref());
    sheet::Settings {
        title: t("workspace-settings-title"),
        name: workspace.map_or_else(
            || tilo_session_core::desktop::names::workspace(""),
            |ws| tilo_session_core::desktop::names::workspace(&ws.name),
        ),
        role: crate::workspace_people::role_line(workspace.and_then(|ws| ws.role_of(person))),
        close: t("workspace-settings-close"),
        tabs: tabs(),
        tab,
        general: general(tree, person, personal, view),
        people: workspace.map_or_else(sheet::People::default, |ws| {
            let workspace = ws.id().to_string();
            people(
                ws,
                &names,
                person,
                &view.map_or_else(Vec::new, |view| view.holders(&workspace)),
            )
        }),
        permissions: workspace.map_or_else(sheet::Permissions::default, |ws| {
            permissions(
                ws,
                person,
                apps,
                engine.filter(|_| tab == sheet::Tab::Permissions),
                &*records.documents,
            )
        }),
        // The workspace's agents are Chat's record, opened when the tab is first shown.
        agents: if tab == sheet::Tab::Agents {
            agents.pane(tree, person, (&names, apps), review)
        } else {
            sheet::Agents::default()
        },
        apps: workspace.map_or_else(sheet::Apps::default, |ws| {
            let lists = crate::app_icons::app_lists().unwrap_or_default();
            apps_pane(ws, person, &lists)
        }),
        // The workspace's emoji are Chat's record, opened when the tab is first shown and nowhere
        // else: no other tab reads it.
        emoji: if tab == sheet::Tab::Emoji {
            emoji.pane(tree, person, &names)
        } else {
            sheet::EmojiPane::default()
        },
        notice: None,
    }
}

/// The General tab: the workspace's name and what the person may change of it, inviting people, the
/// workspace at a glance, which of the person's devices show it and its place among theirs, and
/// leaving it.
/// A workspace that is not the person's own has no devices to show it on: it is theirs only while
/// they are in it.
fn general(
    tree: &Layout,
    person: &str,
    personal: Option<&RefCell<Personal>>,
    view: Option<&WorkspaceView>,
) -> sheet::General {
    let workspace = tree.active_workspace();
    let manage = workspace.is_some_and(|workspace| workspace.may_administer(person));
    // Inviting is the workspace's own grant, not an administrator's part: a moderator or an editor
    // it lets invite makes a link like anyone else it allows. The invite sheet asks again when the
    // link is made.
    let invitable = workspace.is_some_and(|workspace| {
        crate::platform::invite::may_invite(view, &workspace.id().to_string(), person)
    });
    // What the workspace holds, in the three measures a person acts on: who is in it, what it gives
    // particular people or apps, and which apps it opens things with.
    let (members, grants, app_rules) = workspace.map_or((0, 0, 0), |workspace| {
        let sandbox = workspace
            .sandbox
            .iter()
            .filter(|cap| cap.effect == Effect::Grant);
        let given = workspace
            .policy
            .grants
            .iter()
            .filter(|grant| grant.grantee.role_of().is_none());
        (
            workspace.roster().len(),
            sandbox.count().saturating_add(given.count()),
            workspace.default_apps.policy.len(),
        )
    });
    let stat =
        |key: &str, count: usize| (t_args(key, &[("count", count.into())]), count.to_string());
    let devices = match (workspace, personal) {
        (Some(workspace), Some(personal)) => personal.borrow().device_choices(workspace.id()),
        _ => Vec::new(),
    };
    let at = workspace.and_then(|workspace| {
        tree.workspaces
            .iter()
            .position(|one| one.id() == workspace.id())
    });
    let last = tree.workspaces.len().saturating_sub(1);
    let going = going(tree, view);
    sheet::General {
        identity: sheet::Group {
            title: t("workspace-settings-identity-title"),
            description: t("workspace-settings-identity-description"),
        },
        name_label: t("workspace-settings-name-label"),
        can_rename: manage,
        rename_note: (!manage).then(|| t("workspace-settings-rename-admin-only")),
        invite: sheet::Group {
            title: t("workspace-settings-invite-title"),
            description: t(if invitable {
                "workspace-settings-invite-description"
            } else {
                "workspace-settings-invite-not-allowed"
            }),
        },
        invite_label: t("workspace-settings-invite-create"),
        can_invite: invitable,
        glance_title: t("workspace-settings-glance-title"),
        stats: vec![
            stat("workspace-settings-stat-members", members),
            stat("workspace-settings-stat-grants", grants),
            stat("workspace-settings-stat-app-rules", app_rules),
        ],
        devices: sheet::Group {
            title: t("workspace-settings-devices-title"),
            description: t("workspace-settings-devices-local"),
        },
        device_rows: devices.iter().map(device_row).collect(),
        // Which servers keep a workspace is chosen over the workspace's own
        // server API; a desktop's sheet shows none.
        servers: None,
        move_labels: [
            t("workspace-settings-move-earlier"),
            t("workspace-settings-move-later"),
        ],
        move_enabled: [at.is_some_and(|at| at > 0), at.is_some_and(|at| at < last)],
        leave: sheet::Group {
            title: t("workspace-settings-leave"),
            description: t("workspace-settings-leave-description"),
        },
        leave_label: t("workspace-settings-leave-action"),
        leavable: going.leavable,
        leave_note: going.leave_note.clone(),
        delete: (going.sole_member && workspace.is_some()).then(|| sheet::Group {
            title: t("workspace-settings-delete"),
            description: t("workspace-settings-delete-description"),
        }),
        delete_label: t("workspace-settings-delete-action"),
        delete_note: going.delete_note,
        delete_decision: going.decision,
    }
}

/// What leaving and deleting offer here, and why not when they do not: deleting is for the person's
/// own workspace alone, one nobody else is in and no other device of theirs holds, so taking it off
/// here takes it everywhere. Nobody else in it also makes leaving say nothing — there is nobody it
/// could stay for — so the two trade places.
struct Going {
    sole_member: bool,
    leavable: bool,
    leave_note: Option<String>,
    delete_note: Option<String>,
    decision: sheet::Decision,
}

fn going(tree: &Layout, view: Option<&WorkspaceView>) -> Going {
    let workspace = tree.active_workspace();
    let sole_member = workspace.is_some_and(|workspace| workspace.roster().len() <= 1);
    let delete_note = workspace.filter(|_| sole_member).and_then(|workspace| {
        view.map_or_else(
            || Some(t("workspace-settings-delete-not-held")),
            |view| view.not_deletable(workspace.id()).map(not_deletable),
        )
    });
    let leavable = tree.workspaces.len() > 1 && !sole_member;
    let name = workspace.map_or_else(
        || tilo_session_core::desktop::names::workspace(""),
        |workspace| tilo_session_core::desktop::names::workspace(&workspace.name),
    );
    let decision = sheet::Decision {
        title: tilo_sheets::words("workspace-delete-title", &[("name", name.as_str())]),
        description: tilo_sheets::word("workspace-delete-body"),
        keep: tilo_sheets::word("workspace-delete-keep"),
        review: String::new(),
        confirm: tilo_sheets::word("workspace-delete-confirm"),
    };
    Going {
        sole_member,
        leavable,
        leave_note: (!leavable).then(|| {
            if sole_member {
                t("workspace-settings-leave-sole-member")
            } else {
                // The shell keeps the person one workspace, so the last one is not theirs to leave.
                t("workspace-settings-leave-last")
            }
        }),
        delete_note,
        decision,
    }
}

/// Why a workspace is not the person's to delete here, in their words.
fn not_deletable(reason: crate::workspace_view::NotDeletable) -> String {
    use crate::workspace_view::NotDeletable;
    match reason {
        NotDeletable::NotHeld => t("workspace-settings-delete-not-held"),
        NotDeletable::OthersInIt => t("workspace-settings-delete-others"),
        NotDeletable::OtherDevice => t("workspace-settings-delete-elsewhere"),
        NotDeletable::NotOwner => t("workspace-settings-delete-not-owner"),
    }
}

/// One of the person's devices as the General tab lists it: named as its own label names it, the
/// one this session runs on saying so.
fn device_row(choice: &DeviceChoice) -> sheet::Device {
    let named = choice.label.trim();
    let label = match (named.is_empty(), choice.here) {
        (true, true) => t("workspace-settings-device-this"),
        (true, false) => t("workspace-settings-device-unnamed"),
        (false, true) => t_args(
            "workspace-settings-device-here",
            &[("device", named.into())],
        ),
        (false, false) => named.to_owned(),
    };
    sheet::Device {
        key: hex::encode(choice.device),
        label,
        toggle: t(if choice.shown {
            "workspace-settings-device-shown"
        } else {
            "workspace-settings-device-hidden"
        }),
        shown: choice.shown,
    }
}

/// Show the workspace in front on one of the person's devices, or keep it off there: the choice is
/// written to the person's own list, which their devices keep in step, and this device honours it
/// at once.
fn toggle_device(
    handle: &DesktopHandle,
    personal: Option<&RefCell<Personal>>,
    device: &str,
) -> bool {
    let Some(personal) = personal else {
        return false;
    };
    let Some(device) = device_key(device) else {
        tracing::debug!(%device, "not a device key: nothing changed");
        return false;
    };
    let Some(workspace_id) = handle.read(|tree| tree.active_workspace().map(Workspace::id)) else {
        return false;
    };
    let personal = personal.borrow();
    let Some(shown) = personal.shown_on(workspace_id, &device) else {
        return false;
    };
    if !shown {
        return personal.set_shown(workspace_id, device, true);
    }
    // The shell keeps the person one workspace: keeping this device's last one off it would leave
    // them with none, and the workspace would stay whatever the choice said.
    if personal.is_this_device(&device) && handle.read(|tree| tree.workspaces.len()) <= 1 {
        return false;
    }
    personal.set_shown(workspace_id, device, false)
}

/// The device `key` names, as the sheet hands it back.
fn device_key(key: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(key).ok()?;
    <[u8; 32]>::try_from(bytes).ok()
}

/// The sheet's tabs, each by the name the strip shows and the shorter one a narrow sheet uses.
fn tabs() -> Vec<sheet::TabLabel> {
    sheet::Tab::ALL
        .into_iter()
        .map(crate::workspace_people::tab_label)
        .collect()
}

/// What the person here is called on their own row.
fn own_name() -> Option<String> {
    ui::local_identity::load().map(|identity| identity.display_name)
}

/// The People tab: who shares the workspace, their roles, the holders of
/// the workspace with what each can read.
fn people(ws: &Workspace, names: &PeopleNames, person: &str, holders: &[Holder]) -> sheet::People {
    let kept: Vec<domain::people::MemberHolder> = holders
        .iter()
        .map(|holder| domain::people::MemberHolder {
            device: holder.device.clone(),
            name: holder.label.clone(),
            reads: holder.reads,
            cloud: holder.cloud,
        })
        .collect();
    crate::workspace_people::tab(domain::people::content(
        ws,
        names,
        person,
        ws.is_owner(person),
        &kept,
    ))
}

/// The Privacy & Security tab: the permissions the workspace gives, who holds each, and the
/// composer giving one more.
fn permissions(
    ws: &Workspace,
    person: &str,
    apps: &dyn domain::AppDirectory,
    engine: Option<&Engine>,
    held: &dyn tilo_peer_state::workspace_documents::DeviceDocuments,
) -> sheet::Permissions {
    let listed = domain::listed_permissions(ws, apps, person);
    let manage = ws.may_administer(person);
    let mut sections: Vec<sheet::Holders> = Vec::new();
    for entry in &listed {
        let permission = sheet::Permission {
            key: entry.id.clone(),
            holder: entry.holder.clone(),
            detail: match &entry.granted_by {
                Some(by) => t_args(
                    "workspace-settings-permission-detail",
                    &[
                        ("allows", entry.allows.as_str().into()),
                        ("grantor", by.as_str().into()),
                    ],
                ),
                None => entry.allows.clone(),
            },
            expiry: entry.expiry.clone(),
        };
        match sections
            .iter_mut()
            .find(|section| section.title == entry.section)
        {
            Some(section) => section.permissions.push(permission),
            None => sections.push(sheet::Holders {
                title: entry.section.clone(),
                permissions: vec![permission],
            }),
        }
    }
    sheet::Permissions {
        intro: sheet::Intro {
            title: t("workspace-settings-permissions-intro-title"),
            help: t("workspace-settings-permissions-intro-detail"),
            authority: if manage {
                t_args(
                    "workspace-settings-active-grants",
                    &[("count", listed.len().into())],
                )
            } else {
                t("workspace-settings-view-only")
            },
        },
        manage,
        composer: sheet::Composer {
            header: t("workspace-settings-grant-header"),
            who: t("workspace-settings-who"),
            who_agent: t("workspace-settings-who-agent"),
            subjects: domain::grant_recipients(ws)
                .iter()
                .filter_map(|subject| {
                    domain::grant_recipient_key(subject).map(|key| sheet::Choice {
                        key,
                        label: subject.display(ws, person, apps),
                    })
                })
                .collect(),
            nobody: t("workspace-settings-nobody-to-grant"),
            what: t("workspace-settings-what-it-allows"),
            permissions: domain::grant_choices()
                .into_iter()
                .map(|(key, label)| sheet::Choice {
                    key: key.to_owned(),
                    label,
                })
                .collect(),
            how_long: t("workspace-settings-expiry-caption"),
            expiries: domain::lifetime_choices()
                .into_iter()
                .map(|(key, label)| sheet::Choice {
                    key: key.to_owned(),
                    label,
                })
                .collect(),
            grant: t("workspace-settings-grant"),
        },
        documents: manage.then(|| document_access(ws, person, apps, engine, held)),
        sections,
        revoke: t("workspace-settings-revoke"),
        empty: (
            t("workspace-settings-permissions-empty-title"),
            t("workspace-settings-permissions-empty-detail"),
        ),
        footnote: (!manage).then(|| t("workspace-settings-owner-only")),
        revoke_decision: sheet::Decision {
            title: t("workspace-settings-revoke-sheet-title"),
            description: t("workspace-settings-revoke-sheet-description"),
            keep: t("workspace-settings-keep-permission"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-revoke-permission"),
        },
    }
}

fn document_access(
    ws: &Workspace,
    person: &str,
    apps: &dyn domain::AppDirectory,
    engine: Option<&Engine>,
    held: &dyn tilo_peer_state::workspace_documents::DeviceDocuments,
) -> sheet::DocumentAccess {
    let workspace = ws.id().to_string();
    // The workspace's records as its copy here holds them, that this device reads: not its own
    // documents, nor a person's private record, which only its person's grant reaches.
    let documents = engine.map_or_else(Vec::new, |engine| {
        let reads = engine.document_read_policy(ws.id());
        let mut documents = held.listing(&workspace).unwrap_or_else(|error| {
            tracing::warn!(%error, %workspace, "the workspace's resources are not listed");
            Vec::new()
        });
        documents.retain(|(app, document_id)| {
            collab_types::workspace_document(app, document_id).is_none()
                && !tilo_action_policy::roles::is_private_record(app, document_id)
                && reads(app, document_id)
        });
        documents
    });
    let mut recipients: Vec<_> = ws
        .policy
        .custom_roles
        .iter()
        .map(|(id, name)| sheet::Choice {
            key: format!("role:{id}"),
            label: name.clone(),
        })
        .collect();
    recipients.extend(
        domain::grant_recipients(ws)
            .into_iter()
            .filter_map(|recipient| {
                let key = domain::grant_recipient_key(&recipient)?;
                key.starts_with("member:").then(|| sheet::Choice {
                    key,
                    label: recipient.display(ws, person, apps),
                })
            }),
    );
    let targets =
        collab_types::document_access_scopes(&ws.policy, &documents, apps.installed_apps());
    let rules = collab_types::document_access_rules(&ws.policy, &targets);
    super::document_access_pane(targets, rules, recipients)
}

/// The Apps & Files tab: which app opens each kind of file and link here,
/// the workspace's choice and the person's own.
fn apps_pane(ws: &Workspace, person: &str, lists: &AppLists) -> sheet::Apps {
    let manage = ws.may_administer(person);
    sheet::Apps {
        intro: sheet::Intro {
            title: t("workspace-settings-apps-intro-title"),
            help: t("workspace-settings-apps-intro-detail"),
            authority: domain::default_apps_status(ws, &lists.candidates),
        },
        header: t("workspace-settings-apps-header"),
        workspace_choice: t("workspace-settings-workspace-choice"),
        your_choice: t("workspace-settings-your-choice"),
        rows: domain::default_app_choices(ws, person, &lists.candidates)
            .into_iter()
            .map(|choice| sheet::AppRow {
                resource: choice.resource.as_str().to_owned(),
                label: choice.label,
                workspace: choice.workspace,
                yours: choice.yours,
            })
            .collect(),
        manage,
        empty: (
            t("workspace-settings-apps-empty-title"),
            t("workspace-settings-apps-empty-detail"),
        ),
        footnote: (!manage).then(|| t("workspace-settings-owner-only")),
    }
}
