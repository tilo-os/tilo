//! Workspace settings: what the sheet shows of the workspace in front, and what the person's
//! choices there change.
//! The sheet shows and asks; this carries out. Every change to the workspace goes through the
//! workspace's own policy functions — the ones the desk's member commands use — so a change the
//! person may not make is refused there, whatever the sheet showed, and the tree is only written
//! when something changed.

use std::cell::{Cell, RefCell};

use collab_types::caps::{Effect, Expiry};
use tilo_action_policy::agent_answers::AnswerScope;
use tilo_action_policy::{Answer, Refusal, Role, Subject};
use tilo_desktop::authority::WorkspaceAuthority as _;
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::collaborators::PeopleNames;
use tilo_desktop::workspace_settings::{self as domain, agent_access};
use tilo_desktop_model::{DesktopTree, UserId, Workspace, WorkspaceId, role_from_id, role_id};
use tilo_desktop_surface::DesktopHandle;
use tilo_desktop_surface::i18n::{t, t_args};
use tilo_sheets::Seat as _;
use tilo_sheets::file_picker::FilePickerSeat;
use tilo_sheets::workspace_settings as sheet;

use crate::engine::Engine;
use crate::lists::AppLists;
use crate::search::SessionSearch;
use crate::workspaces::{DeviceChoice, Personal};

mod agents;
mod emoji;

pub(crate) use agents::Agents as AgentsTab;
pub(crate) use emoji::Emoji as EmojiTab;

/// The workspace's own records the sheet reads: its agents and its emoji,
/// both kept by Chat for every app of the workspace.
pub(crate) struct WorkspaceTabs {
    pub(crate) agents: AgentsTab,
    pub(crate) emoji: EmojiTab,
}

impl WorkspaceTabs {
    /// The two records, kept in the workspaces' copy of them (`records`), waking the session's loop through `wake` when a
    /// change lands.
    pub(crate) fn new(
        records: &crate::engine_records::EngineRecords,
        wake: tilo_service::Wake,
    ) -> Self {
        Self {
            agents: AgentsTab::new(records.clone(), wake.clone()),
            emoji: EmojiTab::new(records.clone(), wake),
        }
    }
}

/// What the sheet is fed from: the desk in front, the lists its tabs offer,
/// and the workspace's own records its tabs read.
pub(crate) struct Feeding<'a> {
    pub(crate) handle: &'a DesktopHandle,
    pub(crate) person: &'a UserId,
    /// This session's search: the machine's app lists, and the apps a page or a link the sheet asks
    /// for opens in.
    pub(crate) search: &'a SessionSearch,
    /// The desk revision the panes were last fed at.
    pub(crate) fed: &'a Cell<Option<u64>>,
    /// The workspace's agents and emoji, read and changed through Chat's records.
    pub(crate) tabs: &'a WorkspaceTabs,
    /// The session's file chooser, where a picture to add is chosen.
    pub(crate) picker: Option<&'a FilePickerSeat>,
    pub(crate) personal: Option<&'a RefCell<Personal>>,
    /// This device's engine, where the workspace's own governance is written: what the person
    /// changes of the workspace's people goes there, so every member admits it.
    pub(crate) engine: Option<&'a std::sync::Arc<Engine>>,
    /// Where what the workspace answered a change is taken in, once its signature landed.
    pub(crate) landings: &'a crate::landing::Landings,
}

/// Open the sheet on the workspace in front: its panes as the workspace stands, and its name field
/// ready for a new name.
pub(crate) fn open(seat: Option<&sheet::WorkspaceSettingsSeat>, fed: &Feeding<'_>) {
    let Some(seat) = seat else {
        return;
    };
    show(seat, fed, sheet::Tab::General, None);
    seat.reset();
    seat.presentation().open(false);
}

/// Open the sheet on the workspace `workspace` asks for, on `route`'s tab:
/// what an app naming a route asks of the person's own settings.
///
/// # Errors
/// The person is looking at another workspace — a request never switches
/// what they see — or this session shows no workspace settings.
pub(crate) fn open_route(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    fed: &Feeding<'_>,
    workspace: &str,
    route: runtime_portal::tools::WorkspaceSettingsRoute,
) -> Result<(), String> {
    let seat = seat.ok_or("this session shows no workspace settings")?;
    let front = fed
        .handle
        .read(|tree| tree.active_workspace().map(|front| front.id().to_string()));
    if front.as_deref() != Some(workspace) {
        return Err("workspace settings opens for the workspace in front".to_owned());
    }
    show(seat, fed, tab_of(route), None);
    seat.reset();
    seat.presentation().open(false);
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
pub(crate) fn refresh(seat: Option<&sheet::WorkspaceSettingsSeat>, fed: &Feeding<'_>) {
    let Some(seat) = seat else {
        return;
    };
    if !seat.presentation().is_open() {
        return;
    }
    let shown = seat.settings();
    let emoji_moved = shown.tab == sheet::Tab::Emoji
        && fed
            .handle
            .read(|tree| fed.tabs.emoji.moved(tree, fed.person));
    let agents_moved = shown.tab == sheet::Tab::Agents
        && fed
            .handle
            .read(|tree| fed.tabs.agents.moved(tree, fed.person));
    if !emoji_moved && !agents_moved && fed.fed.get() == Some(fed.handle.revision()) {
        return;
    }
    let review = shown
        .agents
        .review
        .as_ref()
        .map(|review| review.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Show the sheet's panes as the desk stands for the person, on `tab` .
fn show(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    tab: sheet::Tab,
    review: Option<&str>,
) {
    let mut shown = settings(
        &fed.handle.read(Clone::clone),
        fed.person,
        &crate::app_icons::app_lists().unwrap_or_default(),
        tab,
        review,
        fed.personal,
        fed.tabs,
        fed.engine.map(|engine| &**engine),
    );
    shown.notice = seat.notice();
    seat.show(shown);
    fed.fed.set(Some(fed.handle.revision()));
}

/// Show the workspace in front on one of the person's devices, or keep it off there: the row says
/// the choice at once, since a workspace kept off this device leaves the desk under the sheet.
fn toggle_workspace_device(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, device: &str) {
    if !toggle_device(fed.handle, fed.personal, device) {
        return;
    }
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Show one agent's review, or the roster again for `None` , on the tab the sheet was on.
fn show_review(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, agent: Option<&str>) {
    show(seat, fed, seat.settings().tab, agent);
}

/// Show the tab the person picked, and keep it for the next opening.
fn show_picked_tab(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, tab: &str) {
    let Some(tab) = sheet::Tab::from_key(tab) else {
        return;
    };
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, tab, review.as_deref());
}

/// Where a member finds a bot for the workspace: the Store's bots, whose pages ask the workspace's
/// host to run one.
const BOTS_LINK: &str = "tilo-store:bots";

/// Carry out what the person asked for in the sheet.
pub(crate) fn follow(
    seat: Option<&sheet::WorkspaceSettingsSeat>,
    fed: &Feeding<'_>,
    intents: &RefCell<Vec<ChromeIntent>>,
) {
    let Some(seat) = seat else {
        return;
    };
    let handle = fed.handle;
    let person = fed.person;
    // What came of the chooser and of the pictures waiting for the image tools, and what the
    // agents' record owes this device, before what the person asks now.
    handle.read(|tree| fed.tabs.emoji.settle(tree, person, fed.picker));
    handle.read(|tree| fed.tabs.agents.settle(tree, person));
    for effect in seat.take_asked() {
        ask(seat, fed, intents, effect);
    }
}

/// Carry out the one ask `effect` names. Every ask the sheet can make is answered here, so an
/// effect added to the sheet is a compile error until the session says what it does with it.
fn ask(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    intents: &RefCell<Vec<ChromeIntent>>,
    effect: sheet::WorkspaceSettingsEffect,
) {
    let handle = fed.handle;
    let person = fed.person;
    match effect {
        sheet::WorkspaceSettingsEffect::ShowTab { tab } => {
            show_picked_tab(seat, fed, &tab);
        }
        sheet::WorkspaceSettingsEffect::DismissNotice => dismiss_notice(seat, fed),
        // Closing is the sheet's own.
        sheet::WorkspaceSettingsEffect::Close => {}
        sheet::WorkspaceSettingsEffect::AddAgent {
            name,
            model,
            colour,
        } => add_agent(seat, fed, &name, &model, &colour),
        sheet::WorkspaceSettingsEffect::ToggleDevice { device } => {
            toggle_workspace_device(seat, fed, &device);
        }
        sheet::WorkspaceSettingsEffect::Rename { name } => {
            intent(intents, ChromeIntent::RenameWorkspace { name });
        }
        sheet::WorkspaceSettingsEffect::Move { step } => move_workspace(handle, step),
        sheet::WorkspaceSettingsEffect::SetRole { member, role } => {
            let Some(role) = role_from_id(&role) else {
                tracing::debug!(%role, "no such role: nothing changed");
                return;
            };
            govern(seat, fed, move |ws, actor| {
                domain::set_member_role(ws, actor, &member, role)
            });
        }
        sheet::WorkspaceSettingsEffect::RemoveMember { member } => {
            govern(seat, fed, move |ws, actor| {
                domain::remove_member(ws, actor, &member)
            });
        }
        sheet::WorkspaceSettingsEffect::Grant {
            subject,
            permission,
            expiry,
        } => govern(seat, fed, move |ws, actor| {
            let Some(draft) = domain::draft_for(ws, &subject, &permission, &expiry) else {
                tracing::debug!(%subject, %permission, "no such grant: nothing changed");
                return Ok(domain::PolicyEdit::default());
            };
            domain::apply_grant_draft(ws, actor, &draft)
        }),
        sheet::WorkspaceSettingsEffect::Revoke { grant } => {
            govern(seat, fed, move |ws, actor| {
                domain::apply_revoke(ws, actor, &grant)
            });
        }
        sheet::WorkspaceSettingsEffect::ReviewActivity => {
            intent(intents, ChromeIntent::ShowActivity(None));
        }
        sheet::WorkspaceSettingsEffect::Invite => intent(intents, ChromeIntent::Invite),
        sheet::WorkspaceSettingsEffect::NextWorkspaceDefault { resource } => {
            next_workspace_default(seat, fed, &resource);
        }
        sheet::WorkspaceSettingsEffect::NextPersonalDefault { resource } => {
            next_personal_default(seat, fed, &resource);
        }
        // The workspace leaves this device's desktop; it stays for everyone in it, on their
        // devices.
        sheet::WorkspaceSettingsEffect::Delete => intent(intents, ChromeIntent::LeaveWorkspace),
        // The person answered the sheet's own question: the desk deletes the workspace with
        // everything in it, and tells the session it went.
        sheet::WorkspaceSettingsEffect::DeleteWorkspace => {
            intent(intents, ChromeIntent::DeleteWorkspaceConfirmed);
        }
        sheet::WorkspaceSettingsEffect::AddEmoji
        | sheet::WorkspaceSettingsEffect::RemoveEmoji { .. }
        | sheet::WorkspaceSettingsEffect::DismissEmojiNotice => {
            handle.read(|tree| fed.tabs.emoji.asked(&effect, tree, person, fed.picker));
        }
        sheet::WorkspaceSettingsEffect::ReviewAgent { agent } => {
            show_review(seat, fed, Some(&agent));
        }
        sheet::WorkspaceSettingsEffect::EndReview => show_review(seat, fed, None),
        sheet::WorkspaceSettingsEffect::SetAgentAnswer {
            agent,
            scope,
            kind,
            answer,
        } => answer_agent(handle, person, &agent, &scope, &kind, &answer),
        sheet::WorkspaceSettingsEffect::ResetAgentApp { agent, scope } => {
            reset_agent_app(handle, person, &agent, &scope);
        }
        sheet::WorkspaceSettingsEffect::RemoveAgent { agent } => {
            retire_agent(seat, fed, &agent);
        }
        sheet::WorkspaceSettingsEffect::SetAgentModel { agent, model } => {
            set_agent_model(seat, fed, &agent, &model);
        }
        sheet::WorkspaceSettingsEffect::SetAgentSpendLimit { agent, dollars } => {
            set_agent_limit(seat, fed, &agent, &dollars);
        }
        sheet::WorkspaceSettingsEffect::SetAgentColour { agent, colour } => {
            set_agent_colour(seat, fed, &agent, &colour);
        }
        sheet::WorkspaceSettingsEffect::AddBot => add_bot(fed),
        sheet::WorkspaceSettingsEffect::RemoveBot { bot } => withdraw_bot(seat, fed, &bot),
    }
}

/// Put the sheet's notice away and draw the tab it stands over again, review and all.
fn dismiss_notice(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>) {
    seat.dismiss_notice();
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Say what the desk does next, for the sheet.
fn intent(intents: &RefCell<Vec<ChromeIntent>>, what: ChromeIntent) {
    intents.borrow_mut().push(what);
}

/// Open the Store's bots, where a member finds one and asks the workspace's host to run it, in the
/// workspace in front.
fn add_bot(fed: &Feeding<'_>) {
    let Some(apps) = fed.search.apps() else {
        tracing::debug!("this session opens no apps: no bot is found");
        return;
    };
    let Some(workspace) = fed.handle.read(|tree| {
        tree.active_workspace()
            .map(|workspace| workspace.id().to_string())
    }) else {
        return;
    };
    if let Err(error) = apps.open_uri(&workspace, BOTS_LINK) {
        tracing::warn!(%error, "the bot directory does not open");
    }
}

/// Take back the workspace's request for the bot at `entry` , as this device: the workspace's host
/// stops running it from then on. A bot the workspace's host runs from its own choice is not a
/// member's to remove, and the sheet offers no control for it.
fn withdraw_bot(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, entry: &str) {
    let Some(workspace_id) = fed
        .handle
        .read(|tree| tree.active_workspace().map(Workspace::id))
    else {
        return;
    };
    let Some(engine) = governing(fed, workspace_id) else {
        tracing::debug!("this device holds no governance to withdraw a bot request in");
        return;
    };
    let entry = entry.to_owned();
    let withdrawn = crate::landing::off_loop(async move {
        crate::membership::Membership::of(&engine, workspace_id)
            .withdraw_bot_request(&entry)
            .await
    });
    let seat = seat.clone();
    fed.landings.then(withdrawn, move |withdrawn| {
        if let Err(reason) = answered(withdrawn) {
            refused(&seat, &reason);
        }
    });
}

/// This device's engine, when it holds the governance of `workspace`.
fn governing(fed: &Feeding<'_>, workspace: WorkspaceId) -> Option<std::sync::Arc<Engine>> {
    fed.engine
        .filter(|engine| engine.policy(&workspace.to_string()).is_ok())
        .cloned()
}

/// What the workspace answered a change, once it landed.
fn answered(landed: Option<Result<(), String>>) -> Result<(), String> {
    landed.unwrap_or_else(|| Err("the change was given up before the workspace answered".into()))
}

/// Say over the sheet why the workspace refused a change it answered after the pass that asked:
/// the panes stand as they were, and the note goes over them.
fn refused(seat: &sheet::WorkspaceSettingsSeat, reason: &str) {
    seat.refuse(sheet::Notice {
        title: t("workspace-settings-refused"),
        detail: Some(reason.to_owned()),
    });
    let mut shown = seat.settings();
    shown.notice = seat.notice();
    seat.show(shown);
}

/// Apply `edit` to the workspace in front as `person` , and keep the tree when it changed
/// something: the policy functions refuse a change the person may not make, and a refusal changes
/// nothing.
fn edit(
    handle: &DesktopHandle,
    person: &UserId,
    edit: impl FnOnce(&mut Workspace, &UserId) -> domain::PolicyEdit,
) {
    edit_changed(handle, person, |workspace, actor| {
        !edit(workspace, actor).is_empty()
    });
}

/// A change to the workspace's people, made where the workspace keeps them.
/// The workspace's governance is the only source of who shares it, so the change goes there first,
/// signed with this device's key: every member admits what it answers as it admits every change its
/// people make. The desk keeps the change only once the workspace took it; one it refuses leaves
/// the desk as it was, and the sheet says why in the workspace's own words.
/// A workspace whose governance this device does not hold has only the desk: the change is the
/// desk's, as it was before workspaces had governance here.
fn govern(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    edit: impl Fn(&mut Workspace, &UserId) -> Result<domain::PolicyEdit, Refusal> + 'static,
) {
    let handle = fed.handle;
    let person = fed.person;
    let mut tree = handle.read(Clone::clone);
    let Some(workspace) = tree.active_workspace_mut() else {
        return;
    };
    let changed = match edit(workspace, person) {
        Ok(changed) => changed,
        Err(refusal) => return refuse(seat, fed, &refusal.to_string()),
    };
    if changed.is_empty() {
        return;
    }
    let workspace_id = workspace.id();
    let Some(engine) = governing(fed, workspace_id) else {
        // The change the person made is the one that stands, so a refusal they have since
        // corrected has nothing left to say.
        seat.dismiss_notice();
        handle.replace_tree(tree);
        return;
    };
    let writes =
        crate::membership::Membership::of(&engine, workspace_id).changes(workspace, &changed);
    let written = crate::landing::off_loop(async move {
        crate::membership::Membership::of(&engine, workspace_id)
            .write(&writes)
            .await
    });
    let (seat, handle, person) = (seat.clone(), handle.clone(), person.clone());
    fed.landings.then(written, move |written| {
        if let Err(reason) = answered(written) {
            return refused(&seat, &reason);
        }
        seat.dismiss_notice();
        // The desk takes the change as it stands now, which moved while the workspace answered.
        let mut tree = handle.read(Clone::clone);
        let Some(workspace) = tree
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id() == workspace_id)
        else {
            return;
        };
        if edit(workspace, &person).is_ok_and(|changed| !changed.is_empty()) {
            handle.replace_tree(tree);
        }
    });
}

/// Say why the workspace refused a change, over the tab the person asked on,
/// and leave the workspace as it was.
fn refuse(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, reason: &str) {
    seat.refuse(sheet::Notice {
        title: t("workspace-settings-refused"),
        detail: Some(reason.to_owned()),
    });
    let shown = seat.settings();
    let review = shown.agents.review.as_ref().map(|row| row.agent.clone());
    show(seat, fed, shown.tab, review.as_deref());
}

/// Apply `change` to the workspace in front as `person` , and keep the tree when it answers that
/// something changed.
fn edit_changed(
    handle: &DesktopHandle,
    person: &UserId,
    change: impl FnOnce(&mut Workspace, &UserId) -> bool,
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
fn next_workspace_default(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, resource: &str) {
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
        Err(error) => refuse(seat, fed, &error.to_string()),
    }
}

/// Move what opens a kind of file or link for `person` themselves on to the next app that can.
fn next_personal_default(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, resource: &str) {
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
        Err(error) => refuse(seat, fed, &error.to_string()),
    }
}

/// Set what an agent may do for one kind of action at one app, and nothing else: the effect carries
/// the answer the person chose, and an empty one takes the agent back to what its admission says.
fn answer_agent(
    handle: &DesktopHandle,
    person: &UserId,
    agent: &str,
    scope: &str,
    kind: &str,
    answer: &str,
) {
    let (Some(scope), Some(kind)) = (AnswerScope::parse(scope), agent_access::kind_from_key(kind))
    else {
        tracing::debug!(%scope, %kind, "no such answer: nothing changed");
        return;
    };
    let answer = if answer.is_empty() {
        None
    } else {
        let Some(answer) = Answer::parse(answer) else {
            tracing::debug!(%answer, "no such answer: nothing changed");
            return;
        };
        Some(answer)
    };
    edit(handle, person, |ws, actor| {
        agent_access::set_agent_answers(ws, actor, agent, &[(scope, kind, answer)])
    });
}

/// Take every answer the person set for an agent at one app back to what the agent's admission
/// says.
fn reset_agent_app(handle: &DesktopHandle, person: &UserId, agent: &str, scope: &str) {
    let Some(scope) = AnswerScope::parse(scope) else {
        tracing::debug!(%scope, "no such app: nothing changed");
        return;
    };
    edit(handle, person, |ws, actor| {
        let answers: Vec<_> = agent_access::KINDS
            .into_iter()
            .map(|kind| (scope.clone(), kind, None))
            .collect();
        agent_access::set_agent_answers(ws, actor, agent, &answers)
    });
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
        colour: Option<u8>,
    ) -> Result<String, String> {
        let tree = self.handle.read(Clone::clone);
        let at = match workspace_id {
            Some(id) => tree
                .workspaces
                .iter()
                .position(|held| held.id().to_string() == id),
            None => tree
                .workspaces
                .iter()
                .position(|held| held.slot == tree.active),
        };
        let Some(workspace) = at.and_then(|at| tree.workspaces.get(at)) else {
            return Err(t("workspace-settings-agents-unavailable"));
        };
        self.tabs
            .agents
            .create(Some(workspace), self.person, name, model, colour)
    }
}

/// Make an agent called `name` where the workspace keeps its agents: the record is the agent's
/// admission, and Chat's grants decide who may make one.
fn add_agent(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    name: &str,
    model: &str,
    colour: &str,
) {
    let colour =
        ui::tokens::PresenceHue::from_id(colour).and_then(|hue| u8::try_from(hue.slot()).ok());
    // The record is the admission: the agent works here because Chat's record names it and this
    // person keeps it.
    if let Err(reason) = fed.make_agent(None, name, model, colour) {
        refuse(seat, fed, &reason);
    }
}

/// Retire the agent `agent` , and take back what the workspace admitted it with: it takes no new
/// work here, on any device.
fn retire_agent(seat: &sheet::WorkspaceSettingsSeat, fed: &Feeding<'_>, agent: &str) {
    let workspace = workspace_in_front(fed);
    if let Err(reason) = fed
        .tabs
        .agents
        .retire(workspace.as_ref(), fed.person, agent)
    {
        return refuse(seat, fed, &reason);
    }
    edit(fed.handle, fed.person, |ws, actor| {
        domain::remove_agent(ws, actor, agent)
    });
    // The review was of an agent the workspace no longer lists.
    let shown = seat.settings();
    show(seat, fed, shown.tab, None);
}

/// Choose the model an agent plans with, as its keeper: the record holds the choice, and the
/// session applies it to this device's Settings on its next pass over the tab, which is where the
/// agent runner reads it.
fn set_agent_model(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    agent: &str,
    model: &str,
) {
    let workspace = workspace_in_front(fed);
    match fed
        .tabs
        .agents
        .set_model(workspace.as_ref(), fed.person, agent, model)
    {
        Ok(()) => {
            let shown = seat.settings();
            show(seat, fed, shown.tab, Some(agent));
        }
        Err(reason) => refuse(seat, fed, &reason),
    }
}

/// Set what an agent may spend a day, as its keeper: what its field held, in dollars, or no limit
/// at all where it held nothing.
fn set_agent_limit(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    agent: &str,
    dollars: &str,
) {
    let limit_micros = if dollars.is_empty() {
        None
    } else {
        let Some(micros) = ui::format::parse_money(dollars) else {
            refuse(seat, fed, &t("workspace-settings-agent-limit-not-amount"));
            return;
        };
        Some(micros)
    };
    let workspace = workspace_in_front(fed);
    match fed
        .tabs
        .agents
        .set_limit(workspace.as_ref(), fed.person, agent, limit_micros)
    {
        Ok(()) => {
            let shown = seat.settings();
            show(seat, fed, shown.tab, Some(agent));
        }
        Err(reason) => refuse(seat, fed, &reason),
    }
}

/// Draw an agent in the colour its keeper picked, as their devices run it.
fn set_agent_colour(
    seat: &sheet::WorkspaceSettingsSeat,
    fed: &Feeding<'_>,
    agent: &str,
    colour: &str,
) {
    let Some(colour) =
        ui::tokens::PresenceHue::from_id(colour).and_then(|hue| u8::try_from(hue.slot()).ok())
    else {
        tracing::debug!(%colour, "no such presence colour: nothing changed");
        return;
    };
    let workspace = workspace_in_front(fed);
    match fed
        .tabs
        .agents
        .set_colour(workspace.as_ref(), fed.person, agent, colour)
    {
        Ok(()) => {
            let shown = seat.settings();
            show(seat, fed, shown.tab, None);
        }
        Err(reason) => refuse(seat, fed, &reason),
    }
}

/// Put the workspace in front one place earlier or later among this device's workspaces.
fn move_workspace(handle: &DesktopHandle, step: i32) {
    let mut tree = handle.read(Clone::clone);
    let Some(at) = tree
        .workspaces
        .iter()
        .position(|workspace| workspace.slot == tree.active)
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
    tree: &DesktopTree,
    person: &UserId,
    lists: &AppLists,
    tab: sheet::Tab,
    review: Option<&str>,
    personal: Option<&RefCell<Personal>>,
    records: &WorkspaceTabs,
    engine: Option<&Engine>,
) -> sheet::Settings {
    let (agents, emoji) = (&records.agents, &records.emoji);
    let workspace = tree.active_workspace();
    let names = PeopleNames::of(tree, person, own_name().as_deref());
    let apps = Directory;
    sheet::Settings {
        title: t("workspace-settings-title"),
        name: workspace.map_or_else(
            || t("workspace-settings-workspace-fallback"),
            |ws| ws.name.clone(),
        ),
        role: workspace
            .and_then(|ws| ws.role_of(person))
            .map_or_else(|| t("workspace-settings-role-guest"), domain::role_label),
        close: t("workspace-settings-close"),
        tabs: tabs(),
        tab,
        general: general(tree, person, personal, engine),
        people: workspace.map_or_else(sheet::People::default, |ws| people(ws, &names, person)),
        permissions: workspace.map_or_else(sheet::Permissions::default, |ws| {
            permissions(ws, person, &apps)
        }),
        // The workspace's agents are Chat's record, opened when the tab is first shown.
        agents: if tab == sheet::Tab::Agents {
            agents.pane(tree, person, &names, &apps, review)
        } else {
            sheet::Agents::default()
        },
        apps: workspace.map_or_else(sheet::Apps::default, |ws| apps_pane(ws, person, lists)),
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
    tree: &DesktopTree,
    person: &UserId,
    personal: Option<&RefCell<Personal>>,
    engine: Option<&Engine>,
) -> sheet::General {
    let workspace = tree.active_workspace();
    let manage = workspace.is_some_and(|workspace| workspace.may_administer(person));
    // Inviting is the workspace's own grant, not an administrator's part: a moderator or an editor
    // it lets invite makes a link like anyone else it allows. The invite sheet asks again when the
    // link is made.
    let invitable = workspace.is_some_and(|workspace| {
        crate::platform::invite::may_invite(engine, &workspace.id().to_string(), person)
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
            .filter(|grant| !matches!(grant.subject, Subject::Role(_)));
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
            .position(|one| one.slot == workspace.slot)
    });
    let last = tree.workspaces.len().saturating_sub(1);
    let going = going(tree, engine);
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

fn going(tree: &DesktopTree, engine: Option<&Engine>) -> Going {
    let workspace = tree.active_workspace();
    let sole_member = workspace.is_some_and(|workspace| workspace.roster().len() <= 1);
    let delete_note = workspace.filter(|_| sole_member).and_then(|workspace| {
        engine.map_or_else(
            || Some(t("workspace-settings-delete-not-held")),
            |engine| engine.not_deletable(workspace.id()).map(not_deletable),
        )
    });
    let leavable = tree.workspaces.len() > 1 && !sole_member;
    let name = workspace.map(|workspace| workspace.name.clone());
    let decision = sheet::Decision {
        title: tilo_sheets::words(
            "workspace-delete-title",
            &[("name", name.as_deref().unwrap_or_default())],
        ),
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
                // The desk keeps the person one workspace, so the last one is not theirs to leave.
                t("workspace-settings-leave-last")
            }
        }),
        delete_note,
        decision,
    }
}

/// Why a workspace is not the person's to delete here, in their words.
fn not_deletable(reason: crate::engine::NotDeletable) -> String {
    use crate::engine::NotDeletable;
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
    let mut personal = personal.borrow_mut();
    let Some(shown) = personal.shown_on(workspace_id, &device) else {
        return false;
    };
    if !shown {
        return personal.set_shown(workspace_id, device, true);
    }
    // The desk keeps the person one workspace: keeping this device's last one off it would leave
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
    let short = [
        "workspace-settings-tab-general",
        "workspace-settings-tab-people",
        "workspace-settings-tab-agents",
        "workspace-settings-tab-privacy",
        "workspace-settings-tab-apps",
        "workspace-settings-tab-emoji",
    ];
    sheet::Tab::ALL
        .into_iter()
        .zip(short)
        .map(|(tab, short)| sheet::TabLabel {
            tab,
            label: t(label_key(tab)),
            short: t(short),
        })
        .collect()
}

/// What a tab is called in the strip.
fn label_key(tab: sheet::Tab) -> &'static str {
    match tab {
        sheet::Tab::General => "workspace-settings-tab-general",
        sheet::Tab::People => "workspace-settings-tab-people",
        sheet::Tab::Agents => "workspace-settings-tab-agents",
        sheet::Tab::Permissions => "workspace-settings-tab-privacy-security",
        sheet::Tab::Apps => "workspace-settings-tab-apps-files",
        sheet::Tab::Emoji => "workspace-settings-tab-emoji",
    }
}

/// What the person here is called on their own row.
fn own_name() -> Option<String> {
    ui::local_identity::load().map(|identity| identity.display_name)
}

/// The People tab: who shares the workspace and their roles.
fn people(ws: &Workspace, names: &PeopleNames, person: &UserId) -> sheet::People {
    let content = domain::people::content(ws, names, person, ws.is_owner(person));
    sheet::People {
        count: content.count,
        status: content.status,
        header: content.header,
        you_tag: content.you_tag,
        remove: content.remove,
        people: content
            .people
            .iter()
            .map(|entry| sheet::Person {
                key: entry.user.clone(),
                name: entry.name.clone(),
                you: entry.you,
                role: entry.role.clone(),
                role_key: ws.role_of(&entry.user).map(|role| role_id(role).to_owned()),
                extra: entry.extra.clone(),
                offers: match entry.offers {
                    domain::people::Offers::Nothing => sheet::Offers::Nothing,
                    domain::people::Offers::Role => sheet::Offers::Role,
                    domain::people::Offers::RoleAndRemove => sheet::Offers::RoleAndRemove,
                },
                can_manage: entry.can_manage,
            })
            .collect(),
        roles: [Role::Viewer, Role::Commenter, Role::Editor]
            .into_iter()
            .map(|role| sheet::RoleChoice {
                key: role_id(role).to_owned(),
                label: domain::role_label(role),
                allows: domain::role_capability_preview(role),
            })
            .collect(),
        empty: content.empty,
        note: content.note,
        remove_decision: sheet::Decision {
            title: t("workspace-settings-remove-sheet-title"),
            description: t("workspace-settings-remove-sheet-description"),
            keep: t("workspace-settings-keep-access"),
            review: t("workspace-settings-review-activity"),
            confirm: t("workspace-settings-remove-person"),
        },
    }
}

/// The Privacy & Security tab: the permissions the workspace gives, who holds each, and the
/// composer giving one more.
fn permissions(
    ws: &Workspace,
    person: &UserId,
    apps: &dyn domain::AppDirectory,
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
            subjects: domain::grant_subjects(ws)
                .iter()
                .filter_map(|subject| {
                    domain::grant_subject_key(subject).map(|key| sheet::Choice {
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
            expiries: domain::expiry_choices()
                .into_iter()
                .map(|(key, label)| sheet::Choice {
                    key: key.to_owned(),
                    label,
                })
                .collect(),
            default_expiry: domain::expiry_key(Expiry::EndOfSession).to_owned(),
            grant: t("workspace-settings-grant"),
        },
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

/// The Apps & Files tab: which app opens each kind of file and link here,
/// the workspace's choice and the person's own.
fn apps_pane(ws: &Workspace, person: &UserId, lists: &AppLists) -> sheet::Apps {
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

/// This machine's applications, as the workspace settings name them.
struct Directory;

impl domain::AppDirectory for Directory {
    fn app_display_name(&self, app_id: &str) -> Option<String> {
        crate::app_names::listed_name(app_id)
    }
}
