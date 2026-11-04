//! What the desktop and its chrome ask the session to do, and the verbs the
//! session answers them with: the queue of asks the desk and the bars leave
//! on every frame, the host actions only a session can carry out (a new
//! workspace, its settings, joining and leaving one), the shortcuts the
//! chrome leaves to its host, and the tray clicks an application answers.

use tilo_actions::ActionAddress;
use tilo_desktop::chrome::ChromeIntent;
use tilo_desktop::desk_verbs::{CHROME_RESOURCE, DESK_RESOURCE};
use tilo_desktop::desktop_host::{
    DesktopAsk, JOIN_WORKSPACE, LEAVE_WORKSPACE, NEW_WORKSPACE, WORKSPACE_SETTINGS,
};
use tilo_desktop::overlays::Slot;
use ui::shortcut_table::ShortcutAction;

use crate::keys::Invocation;
use crate::{
    AppLists, Magnify, SessionCommand, Shared, identity, mount, workspace_settings, workspaces,
};

impl Shared {
    /// Carry out what the desktop asked since the last frame.
    pub(crate) fn follow_asks(&self) {
        // A press on one of the person's devices is one of the session's own
        // verbs: the queue it lands in is carried out here, with the asks.
        self.carry_out_presses();
        let asks = std::mem::take(&mut *self.asks.borrow_mut());
        for ask in asks {
            match ask {
                DesktopAsk::Intent(ChromeIntent::ToggleQuickSettings) => {
                    // The press that closed it is still being delivered: the
                    // same press does not open it again.
                    if self.quick_settings && !self.overlays.dismissed_recently(Slot::QuickSettings)
                    {
                        if self.overlays.is_open(Slot::QuickSettings) {
                            self.overlays.close(Slot::QuickSettings);
                        } else {
                            self.overlays.open(Slot::QuickSettings, None);
                        }
                    }
                }
                DesktopAsk::Intent(ChromeIntent::ShowQuickSettings) => self.show_quick_settings(),
                // The bar's faces, the launcher's Happening now header and a
                // press on someone in a banner all open who is here.
                DesktopAsk::Intent(ChromeIntent::ToggleCollaborators) => {
                    self.people.borrow().toggle();
                }
                DesktopAsk::Intent(ChromeIntent::ShowPerson(person)) => {
                    self.people.borrow().show_person(&person);
                }
                // The floating clock: what the person pressed there is
                // Clocks' own to carry out, or the bar's to put away.
                DesktopAsk::Intent(ChromeIntent::ClockCommand {
                    command,
                    activity_id,
                }) => self.clock.press(&command, activity_id.as_deref()),
                DesktopAsk::Intent(ChromeIntent::HideClock) => self.clock.hide(),
                // A click on an application's tray icon is the
                // application's own to answer.
                DesktopAsk::Intent(ChromeIntent::AppTray { key, request }) => {
                    self.tray_request(&key, request);
                }
                DesktopAsk::Launchpad(action) => self.launchpad(action),
                DesktopAsk::Shortcut(action) => self.shortcut(action),
                DesktopAsk::HostAction(action) => self.host_action(&action),
                DesktopAsk::Music(action) => self.music(action),
                DesktopAsk::LeftWorkspace(name) => workspaces::left_workspace(&self.notices, &name),
                // A window that stopped answering offers to keep waiting for
                // its app or to quit it: the same two choices the desk's own
                // cards make, carried out where the compositor is.
                DesktopAsk::WaitWindow(tab) => {
                    self.handle
                        .liveness_choice(tab, tilo_desktop_surface::LivenessChoice::Wait);
                }
                DesktopAsk::ForceQuitApp(tab) => {
                    self.handle
                        .liveness_choice(tab, tilo_desktop_surface::LivenessChoice::ForceQuit);
                }
                other => tracing::debug!(?other, "desktop ask the session has no part in yet"),
            }
        }
        // What the person chose where only a host performs it: search and a
        // banner hand their host action back here, as the ask on the
        // person's own avatar does.
        for choice in self.handle.take_host_choices() {
            self.host_action(&choice);
        }
        self.follow_sheets();
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
            self.search.perform(&workspace, choice, &mut |own| {
                self.doors.invoke(&own.address, &own.arguments).map(drop)
            });
        }
        // What the session's own actions asked of it.
        self.follow_asks();
    }

    /// Hand a click on the tray icon at `key` to the application that owns
    /// it, as the click reached the bar. An item that went away between the
    /// click and this ask, or a request the application refuses, is said so
    /// in the log rather than passed over in silence: the person saw their
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

    /// Carry out the host action `id` the person chose: what only the
    /// session can do about workspaces, and, for the offers that are not
    /// about workspaces, what answering a question about the person
    /// themselves does ([`identity::answered`]).
    fn host_action(&self, id: &str) {
        match id {
            NEW_WORKSPACE => self.new_workspace(),
            WORKSPACE_SETTINGS => self.open_workspace_settings(),
            JOIN_WORKSPACE => workspaces::open_directory(
                self.directory.as_ref(),
                &self.handle,
                &self.directory_store,
            ),
            LEAVE_WORKSPACE => self.leave_workspace(),
            choice => {
                // A device another of the person's devices found asks here
                // to act as them: the banner's own buttons are its answer.
                if let Some(fabric) = &self.fabric
                    && crate::certification::host_action(fabric, choice)
                {
                    return;
                }
                identity::answered(choice);
            }
        }
    }

    /// The device another of the person's devices found asks here to act as
    /// them. Each ask is put to the person once, on a banner whose two
    /// answers are its host actions
    /// ([`crate::certification::host_action`]); an ask that went — answered
    /// elsewhere, or expired — is taken back from them.
    pub(crate) fn follow_device_asks(&self) {
        use tilo_desktop_surface::i18n::{t, t_args};
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
                t_args("pairing-asks-via", &[("device", asking.via.clone().into())])
            };
            let secs_ago = tilo_clock::unix_now_ms().saturating_sub(asking.asked_at_ms) / 1000;
            let when = ui::format::relative(i64::try_from(secs_ago).unwrap_or(i64::MAX));
            let body = t_args(
                "pairing-asks-body",
                &[
                    ("device", asking.label.clone().into()),
                    ("from", found.into()),
                    ("when", when.into()),
                    ("code", asking.code.clone().into()),
                ],
            );
            let mut notice = Notification::new(t("pairing-asks-title"), body);
            notice.actions = vec![
                (
                    answer(asking.digest, true),
                    t_args("pairing-approve", &[("code", asking.code.clone().into())]),
                ),
                (answer(asking.digest, false), t("pairing-decline")),
            ];
            notice.app_name = Some(t("host-action-source-devices"));
            notice.icon_kind = NotificationIconKind::Permission;
            notice.severity = NotificationSeverity::Info;
            notice.addressed = Some(Addressed::Ask);
            notice.dismiss_after = PERSISTENT_DISMISS;
            self.notices.post(notice);
        }
    }

    /// Show workspace settings for the workspace in front.
    fn open_workspace_settings(&self) {
        workspace_settings::open(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
    }

    /// Keep the tools service's lists current with this pass, and put up the
    /// sheets it left for the loop: workspace settings on a route's tab, and
    /// an app the person asked to uninstall.
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
            self.app_lists(),
            &self.ws_settings_fed,
            &self.workspace_emoji,
            self.file_picker.as_ref(),
            self.personal.as_ref(),
            self.engine.as_deref(),
        )
    }

    /// This machine's applications, as the session's own lists read them.
    fn app_lists(&self) -> std::sync::Arc<AppLists> {
        self.search.app_lists()
    }

    /// Make a new workspace through the desk's own published action, so the
    /// rules and the naming are the desk's.
    fn new_workspace(&self) {
        self.ask_and_report("new-workspace", &serde_json::Map::new());
    }

    /// Leave the workspace the person is in through the desk's own published
    /// action, which takes it off this device and says it went.
    fn leave_workspace(&self) {
        self.ask_and_report("leave-workspace", &serde_json::Map::new());
    }

    /// Carry out the desk's action `action` as the person's ask, with `args`
    /// naming what the action works on.
    pub(crate) fn ask_of_desk(
        &self,
        action: &str,
        args: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        let address = ActionAddress::new(CHROME_RESOURCE, action);
        self.doors
            .invoke(&address, args)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Ask the desk for `action`, and say so when it refuses.
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

    /// A desktop shortcut the chrome left to its host.
    fn shortcut(&self, action: ShortcutAction) {
        let mut invocations = self.invocations.borrow_mut();
        match action {
            ShortcutAction::QuickSettings => {
                if self.quick_settings {
                    if self.overlays.is_open(Slot::QuickSettings) {
                        self.overlays.close(Slot::QuickSettings);
                    } else {
                        self.overlays.open(Slot::QuickSettings, None);
                    }
                }
            }
            // The panel's own pages: it is asked for them once it runs, which
            // is a pass after the stack has it up.
            ShortcutAction::Notifications => {
                self.show_quick_settings();
                self.panel_asks.borrow_mut().push("show-notifications");
            }
            ShortcutAction::ClipboardHistory => {
                self.show_quick_settings();
                self.panel_asks.borrow_mut().push("paste");
            }
            ShortcutAction::Search => {
                invocations.push(Invocation::new(DESK_RESOURCE, "search"));
            }
            ShortcutAction::TabMenu => {
                invocations.push(Invocation::new(mount::shell::WINDOW_MENU, "open"));
            }
            ShortcutAction::Lock => self.commands.borrow_mut().push(SessionCommand::Lock),
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
            other => tracing::debug!(?other, "shortcut the session has no part in yet"),
        }
    }
}
