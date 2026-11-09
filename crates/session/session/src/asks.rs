//! What the desktop and its chrome ask the session to do, and the verbs the session answers them
//! with: the queue of asks the desk and the bars leave on every frame, the host actions only a
//! session can carry out (a new workspace, its settings, joining and leaving one), the shortcuts
//! the chrome leaves to its host, and the tray clicks an application answers.

use tilo_actions::ActionAddress;
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE};
use tilo_desktop::desktop_host::{
    DELETE_WORKSPACE, DesktopAsk, JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, REPORT_PROBLEM,
    SESSION_LOCK, SESSION_LOG_OUT, SESSION_RESTART, SESSION_SHUT_DOWN, WORKSPACE_SETTINGS,
};
use tilo_desktop::launchpad::{LaunchpadAction, LaunchpadAppAction};
use tilo_desktop::overlays::{Anchor, Slot};
use tilo_sheets::power::PowerEffect;
use ui::shortcut_table::{FixedAction, ShortcutAction};

use crate::i18n::{t, t_args};
use crate::keys::Invocation;
use crate::{
    AppLists, Magnify, SessionAction, SessionCommand, Shared, identity, mount, people,
    workspace_settings, workspaces,
};

/// How far a popover stands clear of the caret it was opened from, logical pixels.
const CARET_GAP: f64 = 6.0;

impl Shared {
    /// Carry out what the desktop asked since the last frame.
    pub(crate) fn follow_asks(&self, look: &mut tilo_surface_host::Look<'_>) {
        // A press on one of the person's devices is one of the session's own verbs: the queue it
        // lands in is carried out here, with the asks.
        self.carry_out_presses();
        // What a person's card asked of the session: Chat's conversation with them.
        let asked = self.people.borrow().take_asks();
        for ask in asked {
            match ask {
                people::PeopleAsk::Message { key } => self.message_person(&key),
                people::PeopleAsk::Rename(name) => self.name_self(&name),
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
            }
        }
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for ask in asks {
            match ask {
                DesktopAsk::Intent(intent) => self.bar_ask(intent),
                DesktopAsk::Shortcut(action) => self.shortcut(action),
                DesktopAsk::FixedShortcut(action) => self.fixed_shortcut(action),
                DesktopAsk::HostAction(action) => self.host_action(&action),
                DesktopAsk::Music(action) => self.music(action),
                // A card of Alt+Tab's panel under the pointer, a press on
                // one, or a press away from the panel: the desk's own verbs,
                // as the keys ask for them.
                DesktopAsk::Switcher(action) => self.switcher_ask(action),
                DesktopAsk::LeftWorkspace(name) => workspaces::left_workspace(&self.notices, &name),
                // The desk was asked to delete a workspace, and the person is the one who answers:
                // the question stands in its own dialog, wherever deleting was asked from.
                DesktopAsk::DeleteWorkspace { slot, name } => {
                    self.ask_to_delete_workspace(slot, &name);
                }
                // A workspace the desk deleted, whoever asked it to: what is left of it here in
                // this device's record goes too.
                DesktopAsk::DeletedWorkspace { workspace, name } => {
                    self.deleted_workspace(workspace, &name);
                }
                // A window that stopped answering offers to keep waiting for its app or to quit it:
                // the same two choices the desk's own cards make, carried out where the compositor
                // is.
                DesktopAsk::WaitWindow(tab) => {
                    self.handle
                        .liveness_choice(tab, tilo_desktop_surface::LivenessChoice::Wait);
                }
                DesktopAsk::ForceQuitApp(tab) => {
                    self.handle
                        .liveness_choice(tab, tilo_desktop_surface::LivenessChoice::ForceQuit);
                }
                DesktopAsk::ReportProblem => self.report_problem(),
                // Billing is the person's account: Settings holds it, beside who they are.
                DesktopAsk::AccountSettings => {
                    crate::quick_settings::open_settings(self, "settings://billing");
                }
                // The agent door is the hosting provider's (its MCP front door): a session on the
                // person's own computer serves none, so it holds no grant to take back.
                DesktopAsk::RevokeAgentGrant(grant) => {
                    tracing::warn!(%grant, "no agent door here to revoke the grant at");
                }
            }
        }
        // What the press asked is carried out: what it closed may be raised again, though the press
        // it closed for may already be over—a quick click comes up faster than its own toggle does.
        self.overlays.settle_press();
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

    /// Carry out the rows the person chose in search, and follow a row that asked the session for
    /// something of its own: that is another round of asks.
    fn follow_search_choices(&self, look: &mut tilo_surface_host::Look<'_>) {
        let choices = self.handle.take_search_choices();
        if choices.is_empty() {
            return;
        }
        let workspace = self.handle.read(|tree| {
            tree.active_workspace()
                .map(|held| held.id().to_string())
                .unwrap_or_default()
        });
        for choice in choices {
            let carried = self.search.perform(&workspace, choice.clone(), &mut |own| {
                self.doors
                    .invoke(&own.address, &own.arguments, look)
                    .map(drop)
            });
            if let Err(error) = carried {
                // The row the person chose did not happen: search opens again where it stood,
                // saying why on that row. A row whose search is gone has nothing to answer on, and
                // says it as a notice.
                let why = why_in_words(&error);
                if !self.handle.answer_search(&choice, why) {
                    self.answer_failed(&t("action-failed-that-row"), &error);
                }
            }
        }
        self.follow_asks(look);
    }

    /// What the bar and the desk ask of the chrome's, carried out where only the session can: quick
    /// settings, who is here, the person's own card, an activity's card, an invitation, an
    /// application's tray, and the menu a secondary press on the desk asks the chrome to put at
    /// that place.
    fn bar_ask(&self, intent: ChromeIntent) {
        match intent {
            ChromeIntent::ToggleQuickSettings => {
                // The press that closed it is still being delivered: the same press does not open
                // it again.
                if self.quick_settings && !self.overlays.dismissed_recently(Slot::QuickSettings) {
                    if self.overlays.is_open(Slot::QuickSettings) {
                        self.overlays.close(Slot::QuickSettings);
                    } else {
                        self.surfaces.clear_shell();
                        self.surfaces.clear_panel();
                        self.overlays.open(Slot::QuickSettings, None);
                    }
                }
            }
            ChromeIntent::ShowQuickSettings => self.show_quick_settings(),
            // The bar's faces, the launcher's Happening now header and a press on someone in a
            // banner all open who is here: one surface at a time, so what stood over the desk goes
            // down.
            ChromeIntent::ToggleCollaborators => {
                if !self.people.borrow().is_open() {
                    self.surfaces.clear_shell();
                    self.surfaces.clear_panel();
                }
                self.people.borrow().toggle();
            }
            ChromeIntent::ShowPerson(person) => {
                self.surfaces.clear_shell();
                self.surfaces.clear_panel();
                self.people.borrow().show_person(&person);
            }
            // This person's own avatar in the bar: their own card, the same card as anyone else's,
            // where the roster stands.
            ChromeIntent::ToggleProfile => self.people.borrow().show_self(),
            // Pulling an activity out of the bar's pill: its card is the floating layer's to draw.
            ChromeIntent::ExpandActivity { app, activity } => {
                self.activities.expand(&app, &activity);
            }
            // Inviting people: the link into the workspace in front is made in the invite sheet,
            // wherever it was asked for — the bar's invite button, the roster's footer, workspace
            // settings or the launchpad's own verb.
            ChromeIntent::Invite => self.open_invite(),
            // A secondary press on the desk asks the chrome for the menu of the
            // place it pressed: the same menu the bar's workspace name opens,
            // and the chrome is where it is drawn.
            intent @ ChromeIntent::ToggleWorkspaceMenu { .. } => {
                self.intents.borrow_mut().push(intent);
            }
            // A click on an application's tray icon is the application's own to answer.
            ChromeIntent::AppTray { key, request } => self.tray_request(&key, request),
            other => tracing::debug!(?other, "desk ask the session has no part in yet"),
        }
    }

    /// Open this person's conversation with the person `key` names, in Chat: the app's own
    /// `open-direct-conversation` on its row for the conversation with them, so Chat starts when it
    /// is closed and the conversation comes up with its composer ready, as the desk's Message did
    /// in the browser.
    fn message_person(&self, key: &str) {
        self.invoke_app_action(LaunchpadAppAction::message_person(key));
    }

    /// Name this person, as their own card asked: their active identity record takes the name, so
    /// their own desktop and everyone they work with say it from now on, and this workspace's
    /// presence says it here at once. Says so when the record could not keep it.
    fn name_self(&self, name: &str) {
        use tilo_desktop_surface::notifications::{Notification, NotificationSeverity};

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
                self.notices.post(
                    Notification::new(crate::i18n::t("profile-name-not-saved"), error)
                        .with_severity(NotificationSeverity::Error),
                );
            }
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
        own.status = (!status.is_empty()).then(|| status.to_owned());
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
        let target = tilo_desktop_search::Target::Invoke(Box::new(
            tilo_desktop_search::InvokeTarget::action(
                action.app_id,
                workspace.clone(),
                action.resource,
                action.action,
            ),
        ));
        let carried = self.search.perform(&workspace, target, &mut |own| {
            self.doors
                .invoke(
                    &own.address,
                    &own.arguments,
                    &mut tilo_surface_host::Look::none(),
                )
                .map(drop)
        });
        if let Err(error) = carried {
            self.answer_failed(&action.display_name, &error);
        }
    }

    /// Something a person chose that the session could not carry out, and the
    /// card or link it came from is gone: the person hears it as a notice,
    /// naming what they asked for and why it did not happen.
    pub(crate) fn answer_failed(&self, asked: &str, why: &impl std::fmt::Display) {
        let notice = crate::notices::Notice {
            app: t("action-failed-app"),
            urgency: 1,
            title: t_args("action-failed-title", &[("what", asked.into())]),
            body: t_args("action-failed-body", &[("reason", why.to_string().into())]),
            actions: Vec::new(),
            expires_after: None,
            machine_wide: true,
            sound: None,
        };
        self.notifications.borrow().tell(notice);
    }

    /// What the pointer asked of Alt+Tab's panel: hovering a card reaches it,
    /// a press on one goes there and closes the walk, and a press away from
    /// the panel leaves the person where they were.
    fn switcher_ask(&self, ask: tilo_desktop::window_switcher::SwitcherAsk) {
        use tilo_desktop::window_switcher::SwitcherAsk;
        // The walk is the session desk's, as the keys reach it: the panel's presses ask for the
        // same verbs.
        use tilo_surface_host::shell_part::DESK_RESOURCE as SESSION_DESK;
        let select = |at: usize| {
            let at = i64::try_from(at).unwrap_or(i64::MAX);
            Invocation::new(SESSION_DESK, "select-switcher").with("at", at)
        };
        let mut invocations = self.invocations.borrow_mut();
        match ask {
            SwitcherAsk::Hover(at) => invocations.push(select(at)),
            SwitcherAsk::Choose(at) => {
                invocations.push(select(at));
                invocations.push(Invocation::new(SESSION_DESK, "commit-switcher"));
            }
            SwitcherAsk::Dismiss => invocations
                .push(Invocation::new(SESSION_DESK, "close-surface").with("surface", "switcher")),
        }
    }

    /// Hand a click on the tray icon at `key` to the application that owns it, as the click reached
    /// the bar. An item that went away between the click and this ask, or a request the application
    /// refuses, is said so in the log rather than passed over in silence: the person saw their
    /// click do nothing and this is why.
    fn tray_request(&self, key: &str, request: tilo_desktop::chrome::AppTrayRequest) {
        use tilo_desktop::chrome::AppTrayRequest;
        let Some(tray) = &self.tray else {
            tracing::warn!(key, "no tray is served: the click reaches no application");
            return;
        };
        let request = match request {
            AppTrayRequest::Activate { x, y } => tilo_tray_host::TrayRequest::Activate { x, y },
            AppTrayRequest::SecondaryActivate { x, y } => {
                tilo_tray_host::TrayRequest::SecondaryActivate { x, y }
            }
            AppTrayRequest::ContextMenu { x, y } => {
                tilo_tray_host::TrayRequest::ContextMenu { x, y }
            }
        };
        let named = key.to_owned();
        let refused = tray.request_or_else(key, request, move |error| {
            tracing::warn!(key = %named, %error, "a tray click did not reach its application");
        });
        if let Err(error) = refused {
            tracing::warn!(key, %error, "a tray click did not reach its application");
        }
    }

    /// Carry out the host action `id` the person chose: what only the session can do about
    /// workspaces, and, for the offers that are not about workspaces, what answering a question
    /// about the person themselves does ([`identity::answered`]).
    fn host_action(&self, id: &str) {
        // The empty desk's own verbs: each card publishes a host action, and choosing it anywhere —
        // the card, the palette, an agent — does what the card does.
        if let Some(card) = LaunchpadAction::from_host_id(id) {
            self.launchpad(card);
            return;
        }
        match id {
            NEW_WORKSPACE => self.new_workspace(),
            WORKSPACE_SETTINGS => self.open_workspace_settings(),
            JOIN_WORKSPACE => workspaces::open_directory(
                self.directory.as_ref(),
                &self.handle,
                &self.directory_store,
            ),
            LEAVE_WORKSPACE => self.leave_workspace(),
            // Deleting is the desk's, which asks the person first: the desk's own action is the
            // only thing that deletes.
            DELETE_WORKSPACE => self.ask_desk_to_delete(),
            // The Tilo menu's own rows, and the same effects the power menu's choices are.
            SESSION_LOCK => self.power_effect(PowerEffect::Lock),
            SESSION_LOG_OUT => self.power_effect(PowerEffect::LogOut),
            SESSION_RESTART => self.power_effect(PowerEffect::Restart),
            SESSION_SHUT_DOWN => self.power_effect(PowerEffect::ShutDown),
            REPORT_PROBLEM => self.report_problem(),
            choice => {
                // A device another of the person's devices found asks here to act as them: the
                // banner's own buttons are its answer.
                if let Some(fabric) = &self.fabric
                    && crate::certification::host_action(fabric, choice)
                {
                    return;
                }
                identity::answered(choice);
            }
        }
    }

    /// A report about Tilo itself: the window the person works in opens its box to the one place
    /// this computer sends reports ([`crate::feedback`]), with what it picked on it, and the person
    /// writes what went wrong. A computer told nowhere to send reports says so where the person
    /// asked, rather than closing on nothing.
    fn report_problem(&self) {
        use tilo_desktop_surface::notifications::Notification;

        if self.feedback.is_some() {
            self.report_from.set(true);
            return;
        }
        self.notices.post(Notification::new(
            t("session-feedback-nowhere"),
            t("session-feedback-nowhere-body"),
        ));
    }

    /// The device another of the person's devices found asks here to act as them. Each ask is put
    /// to the person once, on a banner whose two answers are its host actions
    /// ([`crate::certification::host_action`]); an ask that went — answered elsewhere, or expired —
    /// is taken back from them.
    pub(crate) fn follow_device_asks(&self) {
        use tilo_desktop_surface::notifications::{
            Addressed, Notification, NotificationIconKind, NotificationSeverity, PERSISTENT_DISMISS,
        };

        let Some(fabric) = self.fabric.as_ref() else {
            return;
        };
        let (fresh, gone) = crate::certification::newly_asking(fabric.as_ref());
        let answer = |digest: [u8; 32], approve: bool| {
            tilo_desktop_surface::host_action_key(&crate::certification::answer_key(
                digest, approve,
            ))
        };
        for digest in gone {
            for approve in [true, false] {
                self.notices.withdraw_host_action(&answer(digest, approve));
            }
        }
        for asking in fresh {
            let found = if asking.via.is_empty() {
                String::new()
            } else {
                tilo_desktop_surface::i18n::t_args(
                    "pairing-asks-via",
                    &[("device", asking.via.clone().into())],
                )
            };
            let secs_ago = tilo_clock::unix_now_ms().saturating_sub(asking.asked_at_ms) / 1000;
            let when = ui::format::relative(i64::try_from(secs_ago).unwrap_or(i64::MAX));
            let body = tilo_desktop_surface::i18n::t_args(
                "pairing-asks-body",
                &[
                    ("device", asking.label.clone().into()),
                    ("from", found.into()),
                    ("when", when.into()),
                    ("code", asking.code.clone().into()),
                ],
            );
            let mut notice =
                Notification::new(tilo_desktop_surface::i18n::t("pairing-asks-title"), body);
            notice.actions = vec![
                (
                    answer(asking.digest, true),
                    tilo_desktop_surface::i18n::t_args(
                        "pairing-approve",
                        &[("code", asking.code.clone().into())],
                    ),
                ),
                (
                    answer(asking.digest, false),
                    tilo_desktop_surface::i18n::t("pairing-decline"),
                ),
            ];
            notice.app_name = Some(tilo_desktop_surface::i18n::t("host-action-source-devices"));
            notice.icon_kind = NotificationIconKind::Permission;
            notice.severity = NotificationSeverity::Info;
            notice.addressed = Some(Addressed::Ask);
            notice.dismiss_after = PERSISTENT_DISMISS;
            self.notices.post(notice);
        }
    }

    /// Carry out a power effect the person chose: the power menu's rows,
    /// the Tilo menu's and a published action all name the same effects.
    pub(crate) fn power_effect(&self, effect: PowerEffect) {
        match effect {
            PowerEffect::Lock => self.commands.borrow_mut().push(SessionCommand::Lock),
            PowerEffect::LogOut => self.commands.borrow_mut().push(SessionCommand::LogOut),
            PowerEffect::ShutDown if self.hosted => {
                self.commands.borrow_mut().push(SessionCommand::End);
            }
            // The desktop it runs in owns the machine's power and its session switching; the menu
            // does not offer these here, and a published action that asks anyway stops at this
            // session.
            PowerEffect::SwitchUser | PowerEffect::Suspend | PowerEffect::Restart
                if self.hosted =>
            {
                tracing::info!(
                    ?effect,
                    "a session in a window owns no power: nothing was done"
                );
            }
            PowerEffect::SwitchUser => self.session_action(SessionAction::SwitchUser),
            PowerEffect::Suspend => self.session_action(SessionAction::Suspend),
            PowerEffect::Restart => self.session_action(SessionAction::Restart),
            PowerEffect::ShutDown => self.session_action(SessionAction::ShutDown),
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
    /// loop: workspace settings on a route's tab, and an app the person asked to uninstall.
    pub(crate) fn follow_tools(
        &self,
        tools: &mount::tools::Handle,
        media: Vec<mount::drives::Medium>,
    ) {
        tools.saw_apps(&self.app_lists().tools);
        tools.saw_media(media);
        for ask in tools.take_asks() {
            let answered = match &ask {
                mount::tools::Ask::WorkspaceSettings {
                    workspace, route, ..
                } => workspace_settings::open_route(
                    self.workspace_settings.as_ref(),
                    &self.workspace_settings_feed(),
                    workspace,
                    *route,
                ),
                mount::tools::Ask::Uninstall { app, .. } => {
                    if self.ask_uninstall(app) {
                        Ok(())
                    } else {
                        Err("this session shows no uninstall question".to_owned())
                    }
                }
            };
            ask.answer(answered);
        }
    }

    /// What workspace settings feeds its panes from.
    pub(crate) fn workspace_settings_feed(&self) -> workspace_settings::Feeding<'_> {
        workspace_settings::Feeding::new(
            &self.handle,
            &self.person,
            &self.search,
            &self.ws_settings_fed,
            &self.workspace_tabs,
            self.file_picker.as_ref(),
            self.personal.as_ref(),
            self.engine.as_deref(),
        )
    }

    /// This machine's applications, as the session's own lists read them.
    fn app_lists(&self) -> std::sync::Arc<AppLists> {
        self.search.app_lists()
    }

    /// Make a new workspace through the desk's own published action, so the rules and the naming
    /// are the desk's.
    fn new_workspace(&self) {
        self.ask_and_report("new-workspace", &serde_json::Map::new());
    }

    /// Leave the workspace the person is in through the desk's own published action, which takes it
    /// off this device and says it went.
    fn leave_workspace(&self) {
        self.ask_and_report("leave-workspace", &serde_json::Map::new());
    }

    /// Carry out the desk's action `action` as the person's ask, with `args` naming what the action
    /// works on.
    pub(crate) fn ask_of_desk(
        &self,
        action: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let address = ActionAddress::new(CHROME_RESOURCE, action);
        self.doors
            .invoke(&address, args, &mut tilo_surface_host::Look::none())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Ask the desk for `action` , and say so when it refuses.
    fn ask_and_report(&self, action: &str, args: &serde_json::Map<String, serde_json::Value>) {
        if let Err(error) = self.ask_of_desk(action, args) {
            self.handle.acknowledge(error);
        }
    }

    fn show_quick_settings(&self) {
        if self.quick_settings {
            self.overlays.open(Slot::QuickSettings, None);
        }
    }

    /// Open the panel where the person types, for the clipboard's copies: beside the caret that
    /// asked for it, and under the tray when no field holds one.
    fn show_quick_settings_beside_caret(&self) {
        if !self.quick_settings {
            return;
        }
        let anchor = self
            .overlays
            .caret()
            .map(|at| Anchor::Beside { at, gap: CARET_GAP });
        self.overlays.open(Slot::QuickSettings, anchor);
    }

    /// Close the quick settings when they are up, open them otherwise.
    fn toggle_quick_settings(&self) {
        if self.quick_settings {
            if self.overlays.is_open(Slot::QuickSettings) {
                self.overlays.close(Slot::QuickSettings);
            } else {
                self.overlays.open(Slot::QuickSettings, None);
            }
        }
    }

    /// A desktop shortcut the desk left to its host: the device's own answers, and the surfaces
    /// over the desk. Every shortcut is named here or among the desk's, so one the table gains
    /// without an answer does not build.
    fn shortcut(&self, action: ShortcutAction) {
        let mut invocations = self.invocations.borrow_mut();
        match action {
            ShortcutAction::QuickSettings => self.toggle_quick_settings(),
            // The bell opens the one list of notifications: the centre over the desk, whose own
            // Back (or Escape) closes it.
            ShortcutAction::Notifications => {
                invocations
                    .push(Invocation::new(CHROME_RESOURCE, "notifications").with("open", true));
            }
            ShortcutAction::ClipboardHistory => {
                self.show_quick_settings_beside_caret();
                self.panel_asks.borrow_mut().push("paste");
            }
            ShortcutAction::Search => {
                invocations.push(Invocation::new(DESK_RESOURCE, "search"));
            }
            ShortcutAction::TabMenu => {
                invocations.push(Invocation::new(mount::shell::WINDOW_MENU, "open"));
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
            // Asking the person's agents and saying something both send from the window the person
            // works in: its app opens the one box, with no menu to carry it, and the person names
            // whom it goes to. The desk names that window on its next pass, and a desk with none in
            // front leaves the chord to Chat, which a send with nothing to be about has always
            // opened.
            ShortcutAction::AskAgent | ShortcutAction::SaySomething => {
                self.send_from.set(true);
            }
            // The desk answers these on its own windows, strips and places before handing anything
            // on: one arriving here is one it had nothing to act on.
            ShortcutAction::WorkspaceOverview
            | ShortcutAction::EmojiPicker
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
            | ShortcutAction::ReportProblem
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
                tracing::debug!(?action, "a shortcut of the desk's found no window");
            }
        }
    }

    /// A fixed desktop shortcut the desk left to its host: the capture studio, the shortcut guide
    /// and the remote control. Every one the desk does not carry out is named here.
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
            // The desk answers these on its own windows and strips: one arriving here is one it had
            // nothing to act on.
            FixedAction::CloseTab
            | FixedAction::NewWindow
            | FixedAction::DuplicateTab
            | FixedAction::OpenTerminal
            | FixedAction::StripSwitcher
            | FixedAction::WorkspaceHistory
            | FixedAction::ExploreWorkspaceHistory => {
                tracing::debug!(?action, "a fixed shortcut of the desk's found no window");
            }
        }
    }
}

/// Why a row the person chose did not happen, in words they read: the session's own missing app
/// host says what it means, rather than naming its code on the row.
fn why_in_words(error: &tilo_actions::DoorError) -> String {
    match error {
        tilo_actions::DoorError::Unavailable { code, .. } if code == "action_host_unavailable" => {
            tilo_desktop_surface::i18n::t("answer-why-cannot-start")
        }
        other => other.to_string(),
    }
}
