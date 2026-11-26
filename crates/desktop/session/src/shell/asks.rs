//! What the tilolang shell and the sheets over it ask their host to do, and the verbs the session
//! answers them with: the queue of asks the shell leaves on every frame, the requests only the host
//! carries out (who is here, inviting, History, renaming or leaving the workspace), the host
//! actions only a session can carry out (a new workspace, its settings, joining and leaving one),
//! the launchpad's verbs, and the shortcuts the shell leaves to its host.

use tilo_actions::ActionAddress;
use tilo_desktop_model::WorkspaceId;
use tilo_session_core::desktop::host::chrome::ChromeIntent;
use tilo_session_core::desktop::host::contract::{
    BRANCH_WORKSPACE, DELETE_WORKSPACE, DesktopAsk, JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE,
    REPORT_PROBLEM, SESSION_LOCK, SESSION_LOG_OUT, SESSION_RESTART, SESSION_SHUT_DOWN,
    WORKSPACE_SETTINGS,
};
use tilo_session_core::desktop::launchpad::{LaunchpadAction, LaunchpadAppAction};
use tilo_session_core::desktop::navigation::ChromeEffect;
use tilo_session_core::desktop::verbs::{CHROME_RESOURCE, SHELL_RESOURCE};
use ui::search::Scope;
use ui::shortcut_table::{FixedAction, ShortcutAction};

use crate::i18n::{t, t_args};
use crate::keys::Invocation;
use crate::power_menu::Effect;
use crate::{
    BoxAsk, Magnify, SessionAction, SessionCommand, Shared, identity, people, workspace_settings,
    workspaces,
};

impl Shared {
    /// Carry out what the person's cards asked of the session: Chat's conversation with them,
    /// their following, the device they pair, and their own card's words. The roster's own
    /// resource asks through the same queue.
    fn follow_people_asks(&self) {
        // What a person's card asked of the session.
        for ask in self.people_asks.borrow_mut().drain(..) {
            match ask {
                people::PeopleAsk::Message { key } => self.message_person(&key),
                people::PeopleAsk::Agent { who, client, ask } => self.agent_ask(&who, &client, ask),
                people::PeopleAsk::GoTo { who } => self.go_to_person(&who),
                people::PeopleAsk::Follow { who, following } => {
                    if let Some(sync) = &self.sync {
                        sync.set_following(following.then_some(who));
                    }
                }
                people::PeopleAsk::Wave { who } => {
                    if let Some(sync) = &self.sync {
                        sync.wave(&who);
                    }
                }
                people::PeopleAsk::PairDevice => self.open_add_device(),
                people::PeopleAsk::Rename(name) => self.name_self(&name),
                people::PeopleAsk::Picture(picture) => self.picture_self(&picture),
                people::PeopleAsk::Status(status) => self.set_own_status(&status),
                people::PeopleAsk::Availability(chosen) => {
                    let Some(sync) = &self.sync else {
                        continue;
                    };
                    let mut own = sync.own();
                    own.availability = chosen;
                    sync.set_own(own);
                }
                people::PeopleAsk::Colour(id) => {
                    if let Err(error) = settings_store::identity::COLOUR.set(Some(id)) {
                        tracing::warn!(%error, "could not keep the chosen colour");
                    }
                }
                people::PeopleAsk::Settings(link) => {
                    crate::quick_settings::open_settings(self, &link);
                }
                // The person's own card offers what the ask on their avatar asks: choosing it there
                // answers that ask, which goes as it does when they answer it on the avatar.
                people::PeopleAsk::HostAction(id) => {
                    if self
                        .handle
                        .personal_ask()
                        .is_some_and(|ask| ask.action.id == id)
                    {
                        self.handle.answer_personal_ask();
                    } else {
                        self.host_action(&id);
                    }
                }
            }
        }
    }

    /// Carry out what the desktop asked since the last frame.
    pub(crate) fn follow_asks(&self, look: &mut tilo_session_core::surfaces::Look<'_>) {
        // A press on one of the person's devices is one of the session's own verbs: the queue it
        // lands in is carried out here, with the asks.
        self.carry_out_presses();
        // What this device has in front is offered to the person's other devices, as their record
        // of devices carries it.
        self.offer_front();
        // A chip the person pressed opens what the other device showed here.
        self.carry_out_continuity();
        self.follow_people_asks();
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for ask in asks {
            match ask {
                DesktopAsk::Intent(intent) => self.intent(intent),
                DesktopAsk::Shortcut(action) => self.shortcut(action),
                DesktopAsk::FixedShortcut(action) => self.fixed_shortcut(action),
                DesktopAsk::HostAction(action) => self.host_action(&action),
                DesktopAsk::LeftWorkspace(name) => {
                    workspaces::left_workspace(&*self.telling(), &name);
                }
                // The shell was asked to delete a workspace, and the person is the one who answers:
                // the question stands in its own dialog, wherever deleting was asked from.
                DesktopAsk::DeleteWorkspace { workspace, name } => {
                    self.ask_to_delete_workspace(workspace, &name);
                }
                // A workspace the shell deleted, whoever asked it to: what is left of it here in
                // this device's record goes too.
                DesktopAsk::DeletedWorkspace { workspace, name } => {
                    self.deleted_workspace(workspace, &name);
                }
                // The shell named a branch of a workspace: the session makes it, off the loop,
                // and puts it on the shell once what it holds is there.
                DesktopAsk::BranchWorkspace {
                    source,
                    workspace,
                    name,
                } => self.branch_workspace(source, workspace, name),
                // The person named the workspace in front: the name is the
                // workspace's, written where its governance keeps it, so its
                // other devices and every list of it read the same name.
                DesktopAsk::RenamedWorkspace { workspace, name } => {
                    self.renamed_workspace(workspace, name);
                }
                // A window that stopped answering offers to keep waiting for its app or to quit it:
                // the same two choices the shell's own cards make, carried out where the compositor
                // is.
                DesktopAsk::WaitWindow(tab) => {
                    self.handle.liveness_choice(
                        tab,
                        tilo_session_core::desktop::state::LivenessChoice::Wait,
                    );
                }
                DesktopAsk::ForceQuitApp(tab) => {
                    self.handle.liveness_choice(
                        tab,
                        tilo_session_core::desktop::state::LivenessChoice::ForceQuit,
                    );
                }
                DesktopAsk::ReportProblem => self.report_problem(),
                // Settings at the page a panel or a card names: the shell's own
                // search, run here where the workspaces are.
                DesktopAsk::OpenSettings(link) => {
                    crate::quick_settings::open_settings(self, &link);
                }
                // What a `tilo://open#…` link names, shown by the app that keeps it, as a link
                // the person followed lands.
                DesktopAsk::OpenLink(link) => self.open_link(&link),
                // A file shown in Files: the shell's own search opens the app at it.
                DesktopAsk::ShowInFiles(path) => {
                    crate::documents::show_in_files(self, &path);
                }
            }
        }
        // What the press asked is carried out: what it closed may be raised again, though the press
        // it closed for may already be over—a quick click comes up faster than its own toggle does.
        // While a request stands, the press it came from has not been carried out yet and its
        // surface stays down.
        if self.asks.borrow().is_empty() && self.intents.borrow().is_empty() {
            self.overlays.settle_press();
        }
        // One anchored surface at a time, whichever side opened it.
        self.lone_surface();
        // What the person chose where only a host performs it: search and a banner hand their host
        // action back here, as the ask on the person's own avatar does.
        for choice in self.handle.take_host_choices() {
            self.host_action(&choice);
        }
        self.follow_sheets(look);
        self.follow_search_choices(look);
    }

    /// Carry out the rows the person chose in search. A row that asks the session for something of
    /// its own leaves it on the shell's asks, which the pass answers in its next round.
    fn follow_search_choices(&self, look: &mut tilo_session_core::surfaces::Look<'_>) {
        let refused = self.search.perform_choices(&self.handle, &mut |own| {
            self.published
                .invoke(&own.address, &own.arguments, look)
                .map(drop)
        });
        for (choice, error) in refused {
            // The row the person chose did not happen: search opens again where it stood, saying
            // why on that row. A row whose search is gone has nothing to answer on, and says it as
            // a notice.
            if !self
                .handle
                .answer_search(&choice, crate::search::why_in_words(&error))
            {
                self.answer_failed(&t("action-failed-that-row"), &error);
            }
        }
    }

    /// Carry out what the shell's parts and the sheets over it asked of their host since the last
    /// pass. The centre a banner asked for opens too.
    pub(crate) fn follow_intents(&self) {
        let intents = std::mem::take(&mut *self.intents.borrow_mut());
        for intent in intents {
            self.intent(intent);
        }
        if let Some(open) = self.notices.take_centre_ask() {
            let centre = tilo_session_core::desktop::overlays::Overlay::Centre.name();
            if open {
                self.surfaces.open(centre);
            } else {
                self.surfaces.close(centre);
            }
        }
    }

    /// Open what `link`, a `tilo://open#…` address, names: the app that keeps the resource shows
    /// it, started first when no window of it does. What could not be opened is said to the person
    /// in the shell, as a link that lands nowhere is.
    fn open_link(&self, link: &str) {
        let refused = |why: String| self.handle.acknowledge(why);
        let Some(apps) = &self.launch else {
            return refused("No app of this desktop keeps what the link names.".to_owned());
        };
        let workspace = self
            .handle
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()));
        let Some(workspace) = workspace else {
            return refused("A link opens in a workspace, and none is in front.".to_owned());
        };
        match tilo_app::framework::reference::target_of(link) {
            Ok((app, record, reference)) => {
                if let Err(error) =
                    apps.open_reference(&workspace, &app, record.as_deref(), &reference)
                {
                    refused(error.to_string());
                }
            }
            Err(why) => refused(why),
        }
    }

    /// Carry out `intent` where only the session can: who is here, a person's row, an invitation,
    /// another device, History, and the workspace in front's name, leaving and deleting.
    fn intent(&self, intent: ChromeIntent) {
        let refused = |done: Result<(), String>| {
            if let Err(refusal) = done {
                self.handle.acknowledge(refusal);
            }
        };
        match intent {
            // Inviting people: the link into the workspace in front is made in the invite sheet,
            // wherever it was asked for — the roster's footer, workspace settings or the
            // launchpad's own verb.
            ChromeIntent::Invite => self.open_invite(),
            // Another of the person's devices: the ticket it joins with, from Settings' devices
            // page, wherever it was asked for — their own card or Settings' Devices and access.
            ChromeIntent::PairDevice => self.open_add_device(),
            ChromeIntent::ShowActivity => {
                let workspace = self
                    .handle
                    .read(|tree| tree.active_workspace().map(|held| held.id().to_string()));
                if let Some(workspace) = workspace
                    && let Err(error) = self.handle.dispatch(ChromeEffect::Activity {
                        workspace,
                        view: tilo_session_core::desktop::navigation::ActivityView::CatchUp,
                        author: None,
                    })
                {
                    tracing::warn!(%error, "the workspace's history did not open");
                }
            }
            ChromeIntent::RenameWorkspace { name } => {
                let unchanged = self.handle.read(|tree| {
                    tree.active_workspace()
                        .is_some_and(|workspace| workspace.name == name.trim())
                });
                if !unchanged {
                    self.handle.ruled_command(
                        &tilo_desktop_model::commands::DesktopCommand::RenameWorkspace { name },
                    );
                }
            }
            ChromeIntent::LeaveWorkspace => refused(self.handle.leave_workspace(None)),
            ChromeIntent::DeleteWorkspaceConfirmed => {
                refused(self.handle.delete_workspace(None, Some(true)));
            }
        }
    }

    /// A verb of the empty shell's launchpad, as its card, its host action and its resource action
    /// all ask for it: the app action the verb declares, what only the session can do, and else the
    /// app itself. A verb whose app this device does not have opens nothing: the person is shown
    /// the apps it does have, never a window of an app that is not here.
    pub(crate) fn launchpad(&self, action: LaunchpadAction) {
        if !crate::host_actions::offers(&self.lists, action) {
            return self.handle.open_search(Scope::WindowGroup(None), None);
        }
        if let Some(app_action) = action.app_action() {
            return self.invoke_app_action(app_action);
        }
        match action {
            // Bringing people in: the link into this workspace is made in the invite sheet.
            LaunchpadAction::Invite => self.open_invite(),
            LaunchpadAction::Activity => self.intents.borrow_mut().push(ChromeIntent::ShowActivity),
            other => match other.app_target() {
                Some((app, _)) => {
                    let launch = ChromeEffect::OpenApp {
                        app: app.into(),
                        beside: None,
                        background: None,
                        float: None,
                    };
                    if let Err(error) = self.handle.dispatch(launch) {
                        tracing::warn!(%error, app, "launchpad");
                    }
                }
                None => self.handle.open_search(Scope::WindowGroup(None), None),
            },
        }
    }

    /// Open this person's conversation with the person `key` names, in Chat: the app's own
    /// `message_person`, which keeps one conversation with each person, so Chat starts when it
    /// is closed.
    fn message_person(&self, key: &str) {
        self.invoke_app_action(LaunchpadAppAction::message_person(key));
    }

    /// Name this person, as their own card asked: their active identity record takes the name, so
    /// their own desktop and everyone they work with say it from now on, and this workspace's
    /// presence says it here at once. Says so when the record could not keep it.
    fn name_self(&self, name: &str) {
        use crate::platform::notice::Notice;

        let name = name.trim();
        if name.is_empty() {
            return;
        }
        match identity::rename_to(name) {
            Ok(()) => {
                if let Some(sync) = &self.sync {
                    let mut own = sync.own();
                    name.clone_into(&mut own.label);
                    sync.set_own(own);
                }
            }
            Err(error) => {
                tracing::warn!(%error, "could not keep the person's name");
                self.telling().show(&Notice::new(
                    String::new(),
                    2,
                    crate::i18n::t("profile-name-not-saved"),
                    String::new(),
                ));
            }
        }
    }

    /// Give this person the picture `address` names, kept where their identity keeps it, or take
    /// theirs away when it is empty: what every device and app that shows them reads.
    fn picture_self(&self, address: &str) {
        use crate::platform::notice::Notice;

        let workspace = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        });
        let address = address.trim();
        let bytes = if address.is_empty() {
            None
        } else {
            let hash = address
                .strip_prefix("tilo-blob://")
                .or_else(|| address.strip_prefix("tilo.content:"))
                .unwrap_or(address);
            workspace
                .as_deref()
                .and_then(|workspace| self.record_content(workspace, hash))
        };
        if !address.is_empty() && bytes.is_none() {
            self.telling().show(&Notice::new(
                String::new(),
                2,
                t("profile-picture-not-found"),
                String::new(),
            ));
            return;
        }
        if let Err(error) = identity::set_photo(bytes.as_deref()) {
            tracing::warn!(%error, "could not keep the person's picture");
            self.telling().show(&Notice::new(
                String::new(),
                2,
                t("profile-picture-not-saved"),
                String::new(),
            ));
        }
    }

    /// What this person says they are doing, in their own words, beside their face here: clearing
    /// the words stops saying it.
    fn set_own_status(&self, status: &str) {
        let Some(sync) = &self.sync else {
            return;
        };
        let mut own = sync.own();
        let status = status.trim();
        own.status = (!status.is_empty()).then(|| tilo_live::presence::Status {
            text: status.to_owned(),
            automatic: false,
        });
        sync.set_own(own);
    }

    /// Do what an app action names, in the workspace in front (where the person sees what the
    /// action works on): the app starts when it is closed and its action arrives ready to act. A
    /// launchpad card and a person's card ask for their app actions this way.
    pub(crate) fn invoke_app_action(&self, action: LaunchpadAppAction) {
        let Some(workspace) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        }) else {
            // There is no workspace for the app to act in: the person hears it from the card they
            // pressed, which has nothing else to say it on.
            self.answer_failed(&action.display_name, &t("action-failed-no-workspace"));
            return;
        };
        let mut invoked = tilo_desktop_search::InvokeTarget::action(
            action.app_id,
            workspace.clone(),
            action.resource,
            action.action,
        );
        for (name, value) in action.arguments {
            invoked = invoked.with_argument(&name, value);
        }
        let target = tilo_desktop_search::Target::Invoke(Box::new(invoked));
        let carried = self.search.perform(&workspace, target, &mut |own| {
            self.published
                .invoke(
                    &own.address,
                    &own.arguments,
                    &mut tilo_session_core::surfaces::Look::none(),
                )
                .map(drop)
        });
        if let Err(error) = carried {
            self.answer_failed(&action.display_name, &error);
        }
    }

    /// Take the agent grant `grant`, its key in hex, back as every key of the person's is: their
    /// root revokes it and the record of their keys marks it.
    /// It goes through the one dispatch as the person, to the provider of their keys, the way
    /// `tilo device revoke` does: the action asks them once before it runs.
    fn revoke_agent_grant(&self, grant: &str) {
        use tilo_actions::remote::device_tickets;
        let tilo_actions::remote::InvokeArgument::Json(arguments) =
            device_tickets::Revoke::retired(grant).argument()
        else {
            tracing::error!("a revocation is asked with an object");
            return;
        };
        self.invoke_app_action(LaunchpadAppAction {
            // The app the person's keys are published under.
            app_id: "so.tilo.settings".to_owned(),
            display_name: t("action-failed-that-agent"),
            resource: device_tickets::RESOURCE.to_owned(),
            action: device_tickets::REVOKE.to_owned(),
            arguments,
        });
    }

    /// Carry out what a person's card asked of the agent session it shows: a session this device
    /// serves is acted on here, where the connection, its workspace and its run are known; any
    /// other through its agent's card in Console.
    fn agent_ask(
        &self,
        who: &str,
        client: &str,
        ask: tilo_session_core::desktop::launchpad::AgentAsk,
    ) {
        #[cfg(target_os = "linux")]
        if let Some(session) = self.agent_session_of(who) {
            self.agent_session_ask(&session, ask);
            return;
        }
        #[cfg(not(target_os = "linux"))]
        let _ = who;
        self.invoke_app_action(LaunchpadAppAction::ask_agent(client, ask));
    }

    /// What a person's card asked of the agent session `session` this device serves.
    #[cfg(target_os = "linux")]
    fn agent_session_ask(
        &self,
        session: &tilo_action_policy::AgentSessionId,
        ask: tilo_session_core::desktop::launchpad::AgentAsk,
    ) {
        use tilo_session_core::desktop::launchpad::AgentAsk;
        let Some(working) = self.agent_sessions.working(session) else {
            return;
        };
        match ask {
            // Its card of its own is the one the person is looking at: nothing to open.
            AgentAsk::Card => {}
            AgentAsk::Stop => {
                self.agent_sessions.stop(session);
            }
            AgentAsk::UndoSession => self.undo_session_run(&working),
            AgentAsk::TakeOver => {
                // The person picks up where it was: the session ends, and the app it was working
                // in comes forward, as the history's own opening does.
                self.agent_sessions.stop(session);
                self.open_what_it_worked_in(&working);
            }
            AgentAsk::Revoke => {
                // What let its program in here is taken back, and every session of it ends: the
                // next client of that program is asked about, as the first was. A key the
                // session's agent key certified is revoked among the person's keys.
                let key = self
                    .mcp_server
                    .borrow_mut()
                    .as_mut()
                    .and_then(|server| server.revoke(&working.program));
                if let Some(key) = key {
                    self.revoke_agent_grant(&key);
                }
                self.agent_sessions.stop_program(&working.program);
            }
        }
    }

    /// The agent session this device serves that the presence member `who` speaks for, by its
    /// key.
    #[cfg(target_os = "linux")]
    fn agent_session_of(&self, who: &str) -> Option<tilo_action_policy::AgentSessionId> {
        let sync = self.sync.as_ref()?;
        self.agent_sessions
            .spoken_as(*sync.publisher().as_bytes(), who)
    }

    /// Take back what an agent session did here: its run in the workspace that holds it, undone in
    /// each record it changed, as the history's own Undo run does.
    #[cfg(target_os = "linux")]
    fn undo_session_run(&self, working: &crate::agent_sessions::Working) {
        let Some(session) = &working.session else {
            // Its changes were the workspace's own, as any app's: there is no group of them to
            // undo.
            tracing::info!(program = %working.program, "an agent's card asked to undo the changes of a session that has none of its own");
            return;
        };
        if let Err(error) = self.activity.undo_invocation(
            &working.workspace,
            session.as_str(),
            working.changed.clone(),
        ) {
            self.answer_failed(&t("action-failed-that-agent"), &error);
        }
    }

    /// Open what an agent session was working in, in its workspace, as the history opens a
    /// resource: the person continues the work themselves.
    #[cfg(target_os = "linux")]
    fn open_what_it_worked_in(&self, working: &crate::agent_sessions::Working) {
        let Some(app) = working
            .changed
            .last()
            .map(|(app, _)| app.clone())
            .or_else(|| working.app.clone())
        else {
            return;
        };
        let target = ui::HistoryResourceTarget {
            app_id: Some(app.clone()),
            app_label: Some(crate::app_names::app_name(&app)),
            id: String::new(),
            kind: String::new(),
            label: String::new(),
        };
        if let Some(workspace) = tilo_desktop_model::WorkspaceId::parse(&working.workspace) {
            self.open_from_history(&workspace, &target);
        }
    }

    /// Something a person chose that the session could not carry out, and the
    /// card or link it came from is gone: the person hears it as a notice,
    /// naming what they asked for and why it did not happen.
    pub(crate) fn answer_failed(&self, asked: &str, why: &impl std::fmt::Display) {
        let notice = crate::platform::notice::Notice::new(
            t("action-failed-app"),
            1,
            t_args("action-failed-title", &[("what", asked.into())]),
            t_args("action-failed-body", &[("reason", why.to_string().into())]),
        );
        self.notifications.borrow().tell(&notice);
    }

    /// Carry out the host action `id` the person chose: what only the session can do about
    /// workspaces, and, for the offers that are not about workspaces, what answering a question
    /// about the person themselves does ([`identity::answered`]).
    fn host_action(&self, id: &str) {
        // The empty shell's own verbs: each card publishes a host action, and choosing it anywhere —
        // the card, the palette, an agent — does what the card does.
        if let Some(card) = LaunchpadAction::from_host_id(id) {
            self.launchpad(card);
            return;
        }
        match id {
            NEW_WORKSPACE => self.new_workspace(),
            BRANCH_WORKSPACE => self.branch_shown_workspace(),
            WORKSPACE_SETTINGS => self.open_workspace_settings(),
            JOIN_WORKSPACE => crate::directory::open(
                self.directory.as_ref(),
                &self.directory_store,
                &crate::platform::directory::SessionDirectory(self),
            ),
            LEAVE_WORKSPACE => self.leave_workspace(),
            // Deleting is the shell's, which asks the person first: the shell's own action is the
            // only thing that deletes.
            DELETE_WORKSPACE => self.ask_shell_to_delete(),
            // The session's own rows, and the same effects the power menu's choices are.
            SESSION_LOCK => self.power_effect(Effect::Lock),
            SESSION_LOG_OUT => self.power_effect(Effect::LogOut),
            SESSION_RESTART => self.power_effect(Effect::Restart),
            SESSION_SHUT_DOWN => self.power_effect(Effect::ShutDown),
            REPORT_PROBLEM => self.report_problem(),
            // Settings' Devices and access opens Console's Connect page.
            tilo_actions::remote::session_actions::CONNECT_AGENT => {
                self.invoke_app_action(LaunchpadAppAction::connect_agent());
            }
            choice => identity::answered(choice),
        }
    }

    /// A report about Tilo itself: the window the person works in lights what is under the
    /// pointer, and the box opens about what they point at, addressed to the one place this
    /// computer sends reports ([`crate::feedback`]). A computer told nowhere to send reports says
    /// so in that box, which still sends to an agent or a person.
    fn report_problem(&self) {
        self.box_ask.set(Some(BoxAsk::Report));
    }

    /// Carry out a power effect the person chose: the power menu's rows and a
    /// published action name the same effects.
    pub(crate) fn power_effect(&self, effect: Effect) {
        match effect {
            Effect::Lock => self.commands.borrow_mut().push(SessionCommand::Lock),
            Effect::LogOut => self.commands.borrow_mut().push(SessionCommand::LogOut),
            Effect::ShutDown if self.hosted => {
                self.commands.borrow_mut().push(SessionCommand::End);
            }
            // The desktop it runs in owns the machine's power and its session switching; the menu
            // does not offer these here, and a published action that asks anyway stops at this
            // session.
            Effect::SwitchUser | Effect::Suspend | Effect::Restart if self.hosted => {
                tracing::info!(
                    ?effect,
                    "a session in a window owns no power: nothing was done"
                );
            }
            Effect::SwitchUser => self.session_action(SessionAction::SwitchUser),
            Effect::Suspend => self.session_action(SessionAction::Suspend),
            Effect::Restart => self.session_action(SessionAction::Restart),
            Effect::ShutDown => self.session_action(SessionAction::ShutDown),
        }
    }

    /// Ask the system to suspend, restart or shut the machine down.
    pub(crate) fn session_action(&self, action: SessionAction) {
        if let Err(error) = (self.session_actions)(action) {
            tracing::warn!(%error, ?action, "session action");
        }
    }

    /// Show workspace settings for the workspace in front.
    pub(crate) fn open_workspace_settings(&self) {
        workspace_settings::open(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
    }

    /// Keep the tools service's lists current with this pass, and put up the sheets it left for the
    /// loop: workspace settings on a route's tab, and an app the person asked to uninstall. The
    /// identity backup is a layer over the desktop, which only the session raises: its asks come
    /// back for the pass to answer.
    pub(crate) fn follow_tools(
        &self,
        tools: &crate::tools::Handle,
        media: Vec<crate::drives::Medium>,
    ) -> Vec<std::sync::mpsc::SyncSender<Result<(), String>>> {
        let mut backups = Vec::new();
        tools.saw_apps(&crate::app_icons::app_lists().unwrap_or_default().tools);
        tools.saw_media(media);
        for ask in tools.take_asks() {
            match ask {
                // Which app opens a file is decided here, where the shell's workspaces and the
                // person's defaults are read.
                crate::tools::Ask::Openers {
                    path,
                    workspace,
                    reply,
                } => {
                    let _ = reply.send(crate::documents::openers(
                        Some(&self.handle),
                        &workspace,
                        &self.person,
                        &path,
                    ));
                }
                crate::tools::Ask::LinkOpener {
                    url,
                    workspace,
                    reply,
                } => {
                    let _ = reply.send(crate::documents::opener(
                        Some(&self.handle),
                        &workspace,
                        &self.person,
                        &url,
                    ));
                }
                crate::tools::Ask::WorkspaceSettings {
                    workspace,
                    route,
                    reply,
                } => {
                    let answered = workspace_settings::open_route(
                        self.workspace_settings.as_ref(),
                        &self.workspace_settings_feed(),
                        &workspace,
                        route,
                    );
                    let _ = reply.send(answered);
                }
                crate::tools::Ask::Uninstall { app, reply } => {
                    let answered = if self.ask_uninstall(&app) {
                        Ok(())
                    } else {
                        Err("this session shows no uninstall question".to_owned())
                    };
                    let _ = reply.send(answered);
                }
                crate::tools::Ask::IdentityBackup { reply } => backups.push(reply),
            }
        }
        backups
    }

    /// What workspace settings feeds its panes from.
    pub(crate) fn workspace_settings_feed(&self) -> workspace_settings::Feeding<'_> {
        workspace_settings::Feeding {
            handle: &self.handle,
            person: &self.person,
            search: &self.search,
            fed: &self.ws_settings_fed,
            tabs: &self.workspace_tabs,
            asks: Some(&self.asker),
            personal: self.personal.as_ref(),
            view: self.view.as_ref(),
            landings: &self.landings,
        }
    }

    /// Make a new workspace through the shell's own published action, so the rules and the naming
    /// are the shell's.
    fn new_workspace(&self) {
        self.ask_and_report("new-workspace", &serde_json::Map::new());
    }

    /// Branch the workspace the person is in through the shell's own published action, so the rules
    /// and the naming are the shell's.
    fn branch_shown_workspace(&self) {
        let Some(shown) = self.handle.read(|tree| {
            tree.active_workspace()
                .map(tilo_desktop_model::Workspace::id)
        }) else {
            return;
        };
        let args = serde_json::Map::from_iter([(
            "source".to_owned(),
            serde_json::Value::from(shown.to_string()),
        )]);
        self.ask_and_report("branch-workspace", &args);
    }

    /// Leave the workspace the person is in through the shell's own published action, which takes it
    /// off this device and says it went.
    fn leave_workspace(&self) {
        self.ask_and_report("leave-workspace", &serde_json::Map::new());
    }

    /// Carry out the shell's action `action` as the person's ask, with `args` naming what the action
    /// works on.
    pub(crate) fn ask_of_shell(
        &self,
        action: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let address = ActionAddress::new(CHROME_RESOURCE, action);
        self.published
            .invoke(
                &address,
                args,
                &mut tilo_session_core::surfaces::Look::none(),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Ask the shell for `action` , and say so when it refuses.
    fn ask_and_report(&self, action: &str, args: &serde_json::Map<String, serde_json::Value>) {
        if let Err(error) = self.ask_of_shell(action, args) {
            self.handle.acknowledge(error);
        }
    }

    /// Show the quick settings: the shell's surface over the windows.
    fn show_quick_settings(&self) {
        self.surfaces.open(crate::quick_settings::SURFACE);
    }

    /// Close the quick settings when they are up, open them otherwise.
    fn toggle_quick_settings(&self) {
        self.surfaces.toggle(crate::quick_settings::SURFACE);
    }

    /// A desktop shortcut the shell left to its host: the device's own answers, and the surfaces
    /// over the shell. Every shortcut is named here or among the shell's, so one the table gains
    /// without an answer does not build.
    fn shortcut(&self, action: ShortcutAction) {
        let mut invocations = self.invocations.borrow_mut();
        match action {
            ShortcutAction::QuickSettings => self.toggle_quick_settings(),
            // The bell opens the one list of notifications: the centre over the shell, whose own
            // Back (or Escape) closes it.
            ShortcutAction::Notifications => {
                invocations
                    .push(Invocation::new(CHROME_RESOURCE, "notifications").with("open", true));
            }
            // Quick settings, where the shell draws the device's panel. It lists none of the
            // clipboard's copies, so nothing is pasted from it.
            ShortcutAction::ClipboardHistory => self.show_quick_settings(),
            ShortcutAction::Search => {
                invocations.push(Invocation::new(SHELL_RESOURCE, "search"));
            }
            ShortcutAction::TabMenu => {
                invocations.push(Invocation::new(crate::shell::state::WINDOW_MENU, "open"));
            }
            ShortcutAction::Lock => self.commands.borrow_mut().push(SessionCommand::Lock),
            ShortcutAction::PowerMenu => self
                .commands
                .borrow_mut()
                .push(SessionCommand::OpenPowerMenu),
            ShortcutAction::ToggleMagnifier => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::Toggle)),
            ShortcutAction::MagnifierZoomIn => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::ZoomIn)),
            ShortcutAction::MagnifierZoomOut => self
                .commands
                .borrow_mut()
                .push(SessionCommand::Magnifier(Magnify::ZoomOut)),
            ShortcutAction::NextInputSource => self
                .commands
                .borrow_mut()
                .push(SessionCommand::NextInputSource),
            ShortcutAction::DictationToggle
            | ShortcutAction::DictationFinish
            | ShortcutAction::DictationCancel => self.dictation.shortcut(action),
            ShortcutAction::ShortcutHelp => self.shortcut_guide.toggle(),
            // Asking the person's agents, saying something here and reporting a problem all send
            // from the window the person works in: its app opens the one box, with no menu to carry
            // it. Asking an agent addresses the new one; saying something addresses the people
            // here; a report points at what is wrong first and goes to the one place reports go.
            // The shell names that window on its next pass, and a shell with none in front leaves the
            // chord to Chat, which a send with nothing to be about has always opened.
            // The shell draws the picker: it is the shell's view, and its
            // card takes the keys, so the chord opens and closes it there.
            ShortcutAction::EmojiPicker => {
                #[cfg(target_os = "linux")]
                self.surfaces.toggle(crate::device_lists::EMOJI);
            }
            ShortcutAction::AskAgent => self.box_ask.set(Some(BoxAsk::Agent)),
            ShortcutAction::SaySomething => self.box_ask.set(Some(BoxAsk::People)),
            ShortcutAction::ReportProblem => self.box_ask.set(Some(BoxAsk::Report)),
            // The shell answers these on its own windows, strips and places before handing anything
            // on: one arriving here is one it had nothing to act on.
            ShortcutAction::WorkspaceOverview
            | ShortcutAction::FocusPreviousWindow
            | ShortcutAction::FocusNextWindow
            | ShortcutAction::PreviousTab
            | ShortcutAction::NextTab
            | ShortcutAction::PreviousStrip
            | ShortcutAction::NextStrip
            | ShortcutAction::PreviousWorkspace
            | ShortcutAction::NextWorkspace
            | ShortcutAction::MaximizeWindow
            | ShortcutAction::RestoreWindow
            | ShortcutAction::ToggleFloat
            | ShortcutAction::ReopenClosedTab
            | ShortcutAction::ToggleMaximize
            | ShortcutAction::ToggleFullscreen
            | ShortcutAction::DetachTab
            | ShortcutAction::AttachTabLeft
            | ShortcutAction::AttachTabRight
            | ShortcutAction::MoveGroupLeft
            | ShortcutAction::MoveGroupRight
            | ShortcutAction::MoveGroupToPreviousStrip
            | ShortcutAction::MoveGroupToNextStrip
            | ShortcutAction::MoveGroupToPreviousDisplay
            | ShortcutAction::MoveGroupToNextDisplay
            | ShortcutAction::ResizeNarrower
            | ShortcutAction::ResizeWider
            | ShortcutAction::OpenFiles
            | ShortcutAction::OpenTerminal
            | ShortcutAction::FocusGroup1
            | ShortcutAction::FocusGroup2
            | ShortcutAction::FocusGroup3
            | ShortcutAction::FocusGroup4
            | ShortcutAction::FocusGroup5
            | ShortcutAction::FocusGroup6
            | ShortcutAction::FocusGroup7
            | ShortcutAction::FocusGroup8
            | ShortcutAction::FocusGroup9
            | ShortcutAction::FocusStrip1
            | ShortcutAction::FocusStrip2
            | ShortcutAction::FocusStrip3
            | ShortcutAction::FocusStrip4
            | ShortcutAction::FocusStrip5
            | ShortcutAction::FocusStrip6
            | ShortcutAction::FocusStrip7
            | ShortcutAction::FocusStrip8
            | ShortcutAction::FocusStrip9 => {
                tracing::debug!(?action, "a shortcut of the shell's found no window");
            }
        }
    }

    /// A fixed desktop shortcut the shell left to its host: the capture studio, the shortcut guide
    /// and the remote control. Every one the shell does not carry out is named here.
    fn fixed_shortcut(&self, action: FixedAction) {
        use ui::floating_surface::CaptureStudioProtocolMode as Mode;
        match action {
            FixedAction::CaptureScreenshot | FixedAction::PictureInPicture => {
                let mode = if action == FixedAction::CaptureScreenshot {
                    Mode::Screenshot
                } else {
                    Mode::PictureInPicture
                };
                self.invocations.borrow_mut().push(
                    Invocation::new(tilo_capture_studio::RESOURCE, "open")
                        .with("mode", serde_json::to_value(mode).unwrap_or_default()),
                );
            }
            // The last half minute, when the buffer keeps one; starting a recording when it does
            // not, as the studio's own button does when there is nothing to save.
            FixedAction::SaveRecentReplay => self.invocations.borrow_mut().push(Invocation::new(
                tilo_capture_studio::RESOURCE,
                "save-replay",
            )),
            FixedAction::ShortcutGuide => self.shortcut_guide.toggle(),
            FixedAction::StopRemoteInput => self
                .commands
                .borrow_mut()
                .push(SessionCommand::CutRemoteInput),
            // The shell answers these on its own windows and strips: one arriving here is one it had
            // nothing to act on.
            FixedAction::CloseTab
            | FixedAction::NewWindow
            | FixedAction::DuplicateTab
            | FixedAction::OpenTerminal
            | FixedAction::StripSwitcher
            | FixedAction::WorkspaceHistory
            | FixedAction::ExploreWorkspaceHistory => {
                tracing::debug!(?action, "a fixed shortcut of the shell's found no window");
            }
        }
    }
}

impl Shared {
    /// The person named `workspace` on the shell: its name is the workspace's,
    /// so it is written where its governance keeps it, as this device, and
    /// every device and list of it reads it there. A workspace this session
    /// holds no governance of keeps the name the shell gave it.
    fn renamed_workspace(&self, workspace: WorkspaceId, name: String) {
        let Some(view) = self.view.clone().filter(|view| view.holds(workspace)) else {
            return;
        };
        let rename = async move {
            crate::membership::Membership::of(&view, workspace)
                .rename(&name)
                .await
        };
        let handle = self.handle.clone();
        self.landings
            .then(crate::landing::off_loop(rename), move |answer| {
                if let Some(Err(refusal)) = answer {
                    // The shell already shows the new name; the person hears
                    // why the workspace did not take it.
                    handle.acknowledge(refusal);
                }
            });
    }
}
