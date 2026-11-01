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
                .map(|held| held.room().to_string())
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
            choice => identity::answered(choice),
        }
    }

    /// Show workspace settings for the workspace in front.
    fn open_workspace_settings(&self) {
        workspace_settings::open(
            self.workspace_settings.as_ref(),
            &self.workspace_settings_feed(),
        );
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
